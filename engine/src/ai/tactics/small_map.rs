//! 小規模マップV4の行動選択。
//!
//! [`super::v2`] と同じく「盤面 → unit → 移動先 → 評価 → 選択」で組み立て、
//! 小規模マップの作戦が要求する契約を加える。
//! - 首都ルートDAG: 進軍役はDAGのセル列に沿って前進し、回復中は通常の回復探索へ戻る
//! - 占領契約: 専任占領役は割当物件へ近づく移動と、その上での占領を優先する
//! - 自軍生産口: 健全なunitは工場上で手番を終えず、全枠閉塞時は1体を押し出す
//! - 敵生産口の占位: 敵の補充依存度に応じて、生存できる占位を評価する
//! - 間接部隊: 前衛の射点へ追随し、その場で撃てるなら移動より射撃を優先する
//!
//! Squad executorが選んだmember集合（`permitted`）だけを採点し、戦略目標を新たに発明しない。

use super::approach::{ApproachDistances, nearest_approach_bonus, with_turn_distances};
use super::board::{SquadRoles, TacticalBoard, movable_units_and_occupancy};
use super::scoring::{
    BestAction, CAPTURE_SCORE, Rank, SUPPLY_BATTERY_HOLD_BONUS, assess_attack, assess_merge,
    attack_serves_campaign, capture_or_hunt_score, empty_transport_return_score,
    frontline_threat_terrain_score, is_active_supply_battery_at, repair_site_at,
    solo_enemy_approach_score, squad_target_approach_score, stranded_infantry_score,
    terrain_defense_bonus, v3_positioning_score, wait_recovery_score,
};
use super::unit::UnitState;
use crate::ai::command::AiCommand;
use crate::ai::context::tactical::has_capturable_property_at;
use crate::ai::tactics::blockade::{BlockadeTile, EnemyProductionBlockade};
use crate::ai::tactics::campaign::CampaignActionContext;
use crate::ai::tactics::campaign::campaign_action_context;
use crate::ai::tactics::campaign::campaign_mission_allows_capture;
use crate::ai::tactics::campaign::campaign_position_is_on_target_island;
use crate::ai::tactics::candidates::air_move_preserves_return_fuel;
use crate::ai::tactics::scoring::ActionPriority;
use crate::ai::tactics::small_map::squad_executor::coordinated_support_destination;

use crate::ai::squad::{MissionType, SquadManager};
use crate::ai::strategy_profile::{V4StrategyProfile, current_profile};
use crate::ai::turn_distance::TurnDistance;
use crate::ai::v4::hold_line::planner::{self, EligibleHolder, HoldDecision};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, Health, PlayerId};
use crate::resources::Terrain;
use crate::systems::OccupantInfo;
use crate::systems::action::TacticalActionTargets;
use crate::systems::movement::calculate_reachable_tiles;
use crate::systems::production::is_within_production_range;
use bevy_ecs::prelude::*;
use std::collections::BTreeSet;

/// Squad executorから渡されたmember集合（Noneなら全unit）から最良の1手を選び、
/// 評価内訳をEventで通知する。
#[cfg(test)]
pub(crate) fn decide_action(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &HashSet<Entity>,
    permitted: Option<&HashSet<Entity>>,
) -> Option<(Entity, AiCommand)> {
    let batch = evaluate_actions(
        world,
        player_id,
        skip_entities,
        permitted,
        LineHoldMode::Off,
    );
    let mut best = None;
    for (index, action) in batch.actions.iter().enumerate() {
        if best.is_none_or(|previous: usize| action.rank > batch.actions[previous].rank) {
            best = Some(index);
        }
    }
    best.map(|index| batch.choose(world, index))
}

/// 未確定の部隊行動。命令は現在の盤面で合法な既存評価の最良手だけを保持する。
struct EvaluatedAction {
    pub entity: Entity,
    pub origin: GridPosition,
    pub command: AiCommand,
    pub rank: Rank,
    pub movement: Option<execution_order::MovementIntent>,
    hold: Option<HoldDecision>,
    forced_target: Option<GridPosition>,
}

/// 同じ盤面を使って一度だけ評価した候補群。通知は最終選択時にだけ発行する。
struct EvaluatedActions {
    pub actions: Vec<EvaluatedAction>,
    pub board: TacticalBoard,
}

impl EvaluatedActions {
    pub fn choose(mut self, world: &mut World, index: usize) -> (Entity, AiCommand) {
        let action = self.actions.swap_remove(index);
        if let Some(hold) = action.hold {
            hold.trace_execution(world, self.board.player_id);
        }
        if let Some(target) = action.forced_target {
            trace_forced_hold(world, self.board.player_id, || {
                format!(
                    "selected unit={:?} target={target:?} command={:?}",
                    action.entity, action.command
                )
            });
        }
        super::scoring::report_evaluated_action(
            world,
            &self.board,
            action.entity,
            &action.command,
            action.rank.1,
        );
        (action.entity, action.command)
    }
}

/// 陣地線保持の実験スイッチ。既定は `Off`（既存の行動を変えない）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LineHoldMode {
    Off,
    /// 砲座への脅威を減らす配置だけを保持する（CP B0、`OPENWARS_V4_PARTIAL_HOLD_LINE=1`）。
    Partial,
    /// 陣地線の空きマスへ占領担当も含む歩兵を強制的に向かわせる上限実験
    /// （`OPENWARS_V4_FORCE_LINE_HOLD=1`）。
    Forced,
}

impl LineHoldMode {
    /// 保持モードごとの任務の差し替え可否。
    fn allows_mission(
        self,
        mission: Option<&MissionType>,
        campaign: Option<(&MissionType, bool)>,
        explicit_route: bool,
        active_capture: bool,
    ) -> bool {
        match self {
            Self::Off => false,
            Self::Partial => {
                has_only_enemy_pursuit(mission, campaign, explicit_route, active_capture)
            }
            // 上限実験では占領担当も使う。防衛・輸送・出発待ちまでは上書きしない。
            Self::Forced => {
                matches!(
                    mission,
                    None | Some(MissionType::Attack | MissionType::Reserve | MissionType::Capture)
                ) && campaign.is_none_or(|(kind, can_depart)| {
                    can_depart
                        && matches!(
                            kind,
                            MissionType::Attack | MissionType::Reserve | MissionType::Capture
                        )
                })
            }
        }
    }

    /// 環境変数から読む。強制保持を部分保持より優先する。
    pub(super) fn from_env() -> Self {
        if crate::ai::v4::hold_line::forced::enabled() {
            Self::Forced
        } else if std::env::var("OPENWARS_V4_PARTIAL_HOLD_LINE").as_deref() == Ok("1") {
            Self::Partial
        } else {
            Self::Off
        }
    }
}

/// 既存の兵種別候補・採点・所属を変えずに、各部隊の最良手を集める。
/// 依存先を調べるための再帰的な採点や、未実行候補の評価イベント送信は行わない。
fn evaluate_actions(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &HashSet<Entity>,
    permitted: Option<&HashSet<Entity>>,
    hold_mode: LineHoldMode,
) -> EvaluatedActions {
    let (movable_units, occupancy) =
        movable_units_and_occupancy(world, player_id, skip_entities, permitted);
    let manager = world
        .get_resource::<SquadManager>()
        .cloned()
        .unwrap_or_default();
    let squads = SquadRoles::for_small_map(world, player_id, manager);
    let tactical = TacticalBoard::observe(world, player_id, occupancy, squads);
    let board = SmallMapBoard::observe(world, tactical);
    let joint_hold =
        hold_mode == LineHoldMode::Forced && crate::ai::v4::hold_line::forced::with_batteries();
    let battery_sites = if joint_hold {
        forced_forward_battery_sites(world, &board.tactical)
    } else {
        Vec::new()
    };
    let (actions, forced) = with_turn_distances(world, &board.tactical, |world, distances| {
        trace_capture_blocks(world, &board, &movable_units, distances);
        let hold = (hold_mode == LineHoldMode::Partial)
            .then(|| first_hold_decision(world, &board, &movable_units, distances))
            .flatten();
        // 森への出発時期はマスごとに敵の到着手番から決める（forced_line_targets）。
        let forced = (hold_mode == LineHoldMode::Forced)
            .then(|| forced_line_targets(world, &board, &movable_units, distances));
        let batteries = forced_battery_targets(
            world,
            &board.tactical,
            &movable_units,
            &battery_sites,
            distances,
        );
        let actions = movable_units
            .into_iter()
            .filter_map(|entity| {
                let mut unit = SmallMapUnit::observe(world, &board, entity, distances)?;
                let holding = hold.filter(|hold| hold.unit() == entity);
                let battery = batteries.get(&entity).copied();
                let forced_target =
                    battery.or_else(|| forced.as_ref().and_then(|f| f.get(&entity)).copied());
                if let Some(hold) = holding {
                    unit.restrict_to_hold(hold.target());
                } else if let Some(target) = forced_target {
                    unit.march_to_hold(&board, target, distances);
                }
                let mut log = AttackLog {
                    enabled: small_map_trace_enabled(world, player_id),
                    lines: Vec::new(),
                };
                let best = best_action_for_unit_logged(world, &board, &unit, distances, &mut log);
                if log.enabled {
                    eprintln!(
                        "SMALL_MAP_TRACE unit={entity:?} type={:?} at=({},{}) hp={} fuel={} tiles={:?} forced={:?} chosen={:?} attacks={:?}",
                        unit.state.stats.unit_type,
                        unit.state.position.x,
                        unit.state.position.y,
                        unit.state.hp,
                        unit.state.fuel,
                        unit.candidate_tiles,
                        forced_target,
                        best.command().map(|command| (command.clone(), best.rank())),
                        log.lines
                    );
                }
                let best = best
                    .into_inner()
                    .or_else(|| {
                        battery.and_then(|_| unit.wait_for_forced_battery(&board.tactical))
                    });
                if let Some(target) = forced_target {
                    trace_forced_hold(world, player_id, || {
                        format!(
                            "evaluated unit={entity:?} target={target:?} stops={:?} command={:?}",
                            unit.candidate_tiles,
                            best.as_ref().map(|(_, command)| command)
                        )
                    });
                }
                let (rank, command) = best?;
                // 保持側の処理は既に順序付き。退去依存の再探索で停止マスを上書きしない。
                let movement = (holding.is_none() && forced_target.is_none())
                    .then(|| {
                        execution_order::MovementIntent::observe(
                            world, &board, &unit, &command, distances,
                        )
                    })
                    .flatten();
                Some(EvaluatedAction {
                    entity,
                    origin: unit.state.position,
                    command,
                    rank,
                    movement,
                    hold: holding,
                    forced_target,
                })
            })
            .collect();
        (actions, forced)
    });
    // 強制保持の割当は手番をまたいで引き継ぐため、評価のたびに記録し直す。
    if let Some(assignments) = forced {
        crate::ai::v4::hold_line::forced::store(world, player_id, assignments);
    }
    EvaluatedActions {
        actions,
        board: board.tactical,
    }
}

/// 【診断】占領任務の兵が目標へこの手番に届くか、敵に塞がれているかを記録する
/// （`OPENWARS_TRACE_CAPTURE_BLOCK=1` のときだけ。挙動は変えない）。
/// 塞ぐ敵は、1体だけ盤面から外すと届くようになる敵。
fn trace_capture_blocks(
    world: &World,
    board: &SmallMapBoard,
    movable: &[Entity],
    distances: &mut ApproachDistances,
) {
    if std::env::var("OPENWARS_TRACE_CAPTURE_BLOCK").as_deref() != Ok("1") {
        return;
    }
    let tactical = &board.tactical;
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let enemies = tactical
        .action_targets
        .units()
        .iter()
        .filter(|unit| unit.faction != tactical.player_id && !unit.is_transporting)
        .collect::<Vec<_>>();
    let reaches = |occupancy: &HashMap<(usize, usize), OccupantInfo>,
                   state: &UnitState,
                   target: GridPosition| {
        calculate_reachable_tiles(
            &tactical.map,
            occupancy,
            (state.position.x, state.position.y),
            state.stats.movement_type,
            state.stats.max_movement,
            state.fuel,
            tactical.player_id,
            state.stats.unit_type,
            &tactical.registry,
        )
        .contains(&(target.x, target.y))
    };
    for &entity in movable {
        let Some(unit) = SmallMapUnit::observe(world, board, entity, distances) else {
            continue;
        };
        if !unit.state.stats.can_capture || !(unit.is_capture_mission || unit.has_capture_target) {
            continue;
        }
        let Some(target) = unit
            .capture_contract
            .map(|contract| contract.target)
            .or(unit.squad_target)
        else {
            continue;
        };
        let state = &unit.state;
        let owner = tactical
            .properties
            .iter()
            .find(|(position, ..)| *position == target)
            .map(|(_, terrain, owner)| (terrain.as_str().to_string(), owner.map(|p| p.0)));
        let occupant = tactical
            .occupancy
            .get(&(target.x, target.y))
            .map(|occupant| occupant.player_id.0);
        let reach_now = reaches(&tactical.occupancy, state, target);
        let mut own_only = tactical.occupancy.clone();
        own_only.retain(|_, occupant| occupant.player_id == tactical.player_id);
        let reach_without_enemies = reaches(&own_only, state, target);
        let blockers = if !reach_now && reach_without_enemies {
            let mut occupancy = tactical.occupancy.clone();
            enemies
                .iter()
                .filter_map(|enemy| {
                    let tile = (enemy.position.x, enemy.position.y);
                    let removed = occupancy.remove(&tile)?;
                    let opens = reaches(&occupancy, state, target);
                    occupancy.insert(tile, removed);
                    opens.then_some((enemy.entity.to_bits(), tile))
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let turns = distances.turns(&state.stats, state.position, target, 0);
        eprintln!(
            "CAPTURE_BLOCK {}",
            serde_json::json!({
                "turn": turn,
                "player": tactical.player_id.0,
                "unit": entity.to_bits(),
                "at": [state.position.x, state.position.y],
                "hp": state.hp,
                "target": [target.x, target.y],
                "target_property": owner,
                "target_occupant": occupant,
                "reach_now": reach_now,
                "reach_without_enemies": reach_without_enemies,
                "blockers": blockers,
                "turns": turns.turns,
            })
        );
    }
}

/// 盤面上の敵地上部隊の位置（輸送中・航空・艦船は除く）。森マスへの出発合図に使う。
fn enemy_ground_positions(world: &World, board: &TacticalBoard) -> HashSet<GridPosition> {
    enemy_ground_units(world, board)
        .map(|(enemy, _)| enemy.position)
        .collect()
}

/// 盤面上の敵地上部隊（輸送中・航空・艦船は除く）と、その能力値。
fn enemy_ground_units<'a>(
    world: &'a World,
    board: &'a TacticalBoard,
) -> impl Iterator<
    Item = (
        &'a crate::ai::context::tactical::TacticalSnapshotUnit,
        &'a crate::components::UnitStats,
    ),
> {
    use crate::resources::MovementType;
    board
        .action_targets
        .units()
        .iter()
        .filter(|enemy| enemy.faction != board.player_id && !enemy.is_transporting)
        .filter_map(|enemy| {
            let stats = world.get::<crate::components::UnitStats>(enemy.entity)?;
            (!matches!(stats.movement_type, MovementType::Air | MovementType::Ship))
                .then_some((enemy, stats))
        })
}

/// 陣地線の自陣側にある、最前列の所有済み都市を砲座として使う。
fn forced_forward_battery_sites(world: &World, board: &TacticalBoard) -> Vec<GridPosition> {
    let cities = home_cities_by_line_distance(world, board)
        .into_iter()
        .map(|(position, distance, owner)| (position, distance, owner == Some(board.player_id)))
        .collect::<Vec<_>>();
    crate::ai::v4::hold_line::forced::forward_battery_sites(&cities)
}

/// 陣地線の自陣側にある都市と、陣地線までの距離・所有者。陣地線が無ければ空。
fn home_cities_by_line_distance(
    world: &World,
    board: &TacticalBoard,
) -> Vec<(GridPosition, u32, Option<PlayerId>)> {
    let Some(line) = world
        .get_resource::<planner::InitialHoldSites>()
        .and_then(|sites| sites.line(board))
    else {
        return Vec::new();
    };
    board
        .properties
        .iter()
        .filter_map(|(position, terrain, owner)| {
            if *terrain != Terrain::City || !line.home.contains(position) {
                return None;
            }
            let distance = line
                .cells
                .iter()
                .map(|cell| board.map.distance(position.x, position.y, cell.x, cell.y))
                .min()?;
            Some((*position, distance, *owner))
        })
        .collect()
}

/// 試験（`OPENWARS_V4_FORCE_CITY_RECAPTURE=1`）: 上の敵を優先攻撃する非生産前線都市。
/// 前線都市は所有を問わず陣地線に最も近い都市で、敵所有か、敵地上部隊が上にいる自軍都市。
fn forced_recapture_cities(
    world: &World,
    board: &TacticalBoard,
    own_production: &OwnProductionSites,
) -> HashSet<GridPosition> {
    use crate::ai::v4::hold_line::forced;
    if !forced::enabled() || !forced::city_recapture_enabled() {
        return HashSet::default();
    }
    let cities = home_cities_by_line_distance(world, board);
    let front = forced::front_cities(
        &cities
            .iter()
            .map(|(position, distance, _)| (*position, *distance))
            .collect::<Vec<_>>(),
    );
    let enemies = enemy_ground_positions(world, board);
    let states = cities
        .into_iter()
        .filter(|(position, ..)| {
            front.contains(position) && !own_production.contains((position.x, position.y))
        })
        .map(|(position, _, owner)| (position, owner, enemies.contains(&position)))
        .collect::<Vec<_>>();
    forced::lost_cities(&states, board.player_id)
}

/// 砲を空き都市に一門ずつ割り当て、すでに布陣した砲には現位置を優先する。
fn forced_battery_targets(
    world: &World,
    board: &TacticalBoard,
    movable: &[Entity],
    sites: &[GridPosition],
    distances: &mut ApproachDistances,
) -> std::collections::BTreeMap<Entity, GridPosition> {
    use crate::ai::v4::hold_line::forced;
    use crate::resources::UnitType;
    if sites.is_empty() {
        return Default::default();
    }
    let states = movable
        .iter()
        .filter_map(|&entity| UnitState::observe(world, board, entity))
        .filter(|state| {
            !state.combat_ineffective
                && matches!(
                    state.stats.unit_type,
                    UnitType::Artillery
                        | UnitType::LightSpGun
                        | UnitType::HeavySpGun
                        | UnitType::Rockets
                )
        })
        .collect::<Vec<_>>();
    let pool = states.iter().map(|state| state.entity).collect::<Vec<_>>();
    let unavailable = HashSet::default();
    forced::assign(
        &Default::default(),
        &forced::AssignInput {
            line: sites,
            unavailable: &unavailable,
            pool: &pool,
        },
        |_| true,
        |entity, site| {
            let state = states.iter().find(|state| state.entity == entity)?;
            if site != state.position && board.occupancy.contains_key(&(site.x, site.y)) {
                return None;
            }
            let distance = distances.turns(&state.stats, state.position, site, 0);
            Some((distance.turns, distance.used_mp))
        },
    )
}

/// 強制保持の経路・候補・命令を、診断を有効にした対局だけに記録する。
fn trace_forced_hold(world: &World, player: PlayerId, message: impl FnOnce() -> String) {
    if std::env::var("OPENWARS_TRACE_HOLD_LINE").as_deref() != Ok("1") {
        return;
    }
    let turn = world
        .resource::<crate::resources::MatchState>()
        .current_turn_number
        .0;
    eprintln!("FORCED_HOLD turn={turn} player={} {}", player.0, message());
}

/// 強制保持の上限実験: 占領担当も含め、陣地線の空きマスに向かう兵の割当を更新する。
/// 防衛・回復・輸送・出発待ちは従来どおり優先する。
fn forced_line_targets(
    world: &World,
    board: &SmallMapBoard,
    movable: &[Entity],
    distances: &mut ApproachDistances,
) -> std::collections::BTreeMap<Entity, GridPosition> {
    use crate::ai::v4::hold_line::forced;
    let tactical = &board.tactical;
    let Some(line) = world
        .get_resource::<planner::InitialHoldSites>()
        .and_then(|sites| sites.line(tactical))
    else {
        trace_forced_hold(world, tactical.player_id, || "no_line".into());
        return Default::default();
    };
    let previous = forced::previous(world, tactical.player_id);
    let units = movable
        .iter()
        .filter_map(|&entity| SmallMapUnit::observe(world, board, entity, distances))
        .collect::<Vec<_>>();
    // 占領中（捕獲ポイントが減っている拠点の上）の兵は森へ引き抜かない。
    let capturing_sites: HashSet<GridPosition> = world
        .iter_entities()
        .filter_map(|entity| {
            let pos = entity.get::<GridPosition>()?;
            let prop = entity.get::<crate::components::Property>()?;
            forced::is_capturing_in_progress(
                prop.owner_id,
                tactical.player_id,
                prop.capture_points,
                prop.max_capture_points,
            )
            .then_some(*pos)
        })
        .collect();
    let owned_front_cities: HashSet<GridPosition> = forced_forward_battery_sites(world, tactical)
        .into_iter()
        .collect();
    let pool_units = units
        .iter()
        .filter(|unit| {
            let has_capture_duty = unit.is_capture_mission || unit.has_capture_target;
            forced::can_hold_line_in_forced_mode(
                capturing_sites.contains(&unit.state.position),
                previous.contains_key(&unit.state.entity),
                owned_front_cities.contains(&unit.state.position),
                has_capture_duty,
            ) && is_line_holder(world, board, unit, LineHoldMode::Forced)
        })
        .collect::<Vec<_>>();
    let pool = pool_units
        .iter()
        .map(|unit| unit.state.entity)
        .collect::<Vec<_>>();
    let pool_set = pool.iter().copied().collect::<HashSet<_>>();
    let movable_set = movable.iter().copied().collect::<HashSet<_>>();
    // 敵がいるマスと、保持対象でない味方（戦車・間接・占領中の兵など）がいるマスは割り当てない。
    let unavailable = tactical
        .action_targets
        .units()
        .iter()
        .filter(|unit| {
            !unit.is_transporting
                && line.cells.contains(&unit.position)
                && (unit.faction != tactical.player_id
                    || !(pool_set.contains(&unit.entity) || previous.contains_key(&unit.entity)))
        })
        .map(|unit| unit.position)
        .collect::<HashSet<_>>();
    // 行動済みで今は動けない兵の割当は保持する。動ける兵は資格を失ったら外す。
    let retained = |entity: Entity| {
        world.get::<Health>(entity).is_some_and(|hp| hp.current > 0)
            && (!movable_set.contains(&entity) || pool_set.contains(&entity))
    };
    let input = forced::AssignInput {
        line: &line.cells,
        unavailable: &unavailable,
        pool: &pool,
    };
    // 敵地上部隊が入った・隣接したマスだけ、新たに出発させる。それまでは本来の仕事を続ける。
    // ただし所有済みの前線都市上にいる歩兵（占領完了・前線生産）は線へ解放する。
    let enemies = enemy_ground_positions(world, tactical);
    let contested = forced::contested_cells(&line.cells, &enemies, |cell| {
        tactical
            .map
            .get_adjacent(cell.x, cell.y)
            .into_iter()
            .map(|(x, y)| GridPosition { x, y })
            .collect()
    });
    // 試験: 守備兵を外すと敵が次の手番に入れる非生産前線都市では、その兵を都市担当として残す。
    let guards = if forced::city_guard_enabled() {
        let garrisons = pool_units
            .iter()
            .map(|unit| (unit.state.entity, unit.state.position))
            .filter(|(_, position)| {
                owned_front_cities.contains(position)
                    && !board.own_production.contains((position.x, position.y))
            })
            .collect::<Vec<_>>();
        // 占有表の複製は判定ごとではなく1回だけにする。
        let mut occupancy = tactical.occupancy.clone();
        forced::city_guards(&garrisons, |_, city| {
            enemy_can_enter_next_turn(world, tactical, &mut occupancy, city)
        })
    } else {
        Default::default()
    };
    let plan =
        forced::assign_with_city_guards(&previous, &input, guards, retained, |entity, cell| {
            let unit = pool_units.iter().find(|unit| unit.state.entity == entity)?;
            let on_owned_front_city = owned_front_cities.contains(&unit.state.position);
            if !contested.contains(&cell) && !on_owned_front_city {
                return None;
            }
            let distance = distances.turns(&unit.state.stats, unit.state.position, cell, 0);
            Some((distance.turns, distance.used_mp))
        });
    let forced::HoldPlan {
        line: mut assignments,
        guards,
        unfilled,
    } = plan;
    trace_forced_hold(world, tactical.player_id, || {
        let infantry = units
            .iter()
            .filter(|unit| unit.state.stats.can_capture)
            .map(|unit| {
                let state = &unit.state;
                (
                    state.entity,
                    (state.position.x, state.position.y),
                    tactical.squads.mission(state.entity),
                    unit.campaign
                        .as_ref()
                        .map(|c| (&c.mission_type, c.departure_authorized)),
                    unit.is_capture_mission,
                    unit.has_capture_target,
                    unit.route.tactical_target.flatten().is_some()
                        || unit.route.advance_destination.is_some(),
                    state.combat_ineffective,
                )
            })
            .collect::<Vec<_>>();
        format!(
            "assign movable={} observed={} infantry={infantry:?} pool={pool:?} unavailable={unavailable:?} assignments={assignments:?} guards={guards:?} unfilled={unfilled:?} contested={contested:?}",
            movable.len(),
            units.len()
        )
    });
    // 都市担当は保持先＝現在地として渡し、その場の射撃・待機だけを残す。
    assignments.extend(guards);
    assignments
}

/// 都市の占有だけを外したとき、敵地上部隊のいずれかが次の手番にその都市へ入れるか。
/// 実際の地形・移動コスト・燃料・他の占有で判定する（他の自軍部隊はそのまま障害として残す）。
/// `occupancy` は判定の間だけ都市の占有を外し、戻してから返す。
fn enemy_can_enter_next_turn(
    world: &World,
    board: &TacticalBoard,
    occupancy: &mut HashMap<(usize, usize), OccupantInfo>,
    city: GridPosition,
) -> bool {
    let tile = (city.x, city.y);
    let garrison = occupancy.remove(&tile);
    let reachable = enemy_ground_units(world, board).any(|(enemy, stats)| {
        let fuel = world
            .get::<crate::components::Fuel>(enemy.entity)
            .map_or(stats.max_fuel, |fuel| fuel.current);
        calculate_reachable_tiles(
            &board.map,
            occupancy,
            (enemy.position.x, enemy.position.y),
            stats.movement_type,
            stats.max_movement,
            fuel,
            enemy.faction,
            stats.unit_type,
            &board.registry,
        )
        .contains(&tile)
    });
    if let Some(garrison) = garrison {
        occupancy.insert(tile, garrison);
    }
    reachable
}

/// 攻撃Squadへの所属だけでは確約としない。出発待ち・占領・具体的な進軍指令は優先する。
fn has_only_enemy_pursuit(
    mission: Option<&MissionType>,
    campaign: Option<(&MissionType, bool)>,
    explicit_route: bool,
    active_capture: bool,
) -> bool {
    !explicit_route
        && !active_capture
        && matches!(
            mission,
            None | Some(MissionType::Attack | MissionType::Reserve)
        )
        && campaign.is_none_or(|(kind, may_replace)| *kind == MissionType::Attack && may_replace)
}

/// 仮盤面で決めた順序の先頭だけを評価に渡す。実行後は次の実盤面から再計画する。
fn first_hold_decision(
    world: &World,
    board: &SmallMapBoard,
    movable: &[Entity],
    distances: &mut ApproachDistances,
) -> Option<HoldDecision> {
    let units = movable
        .iter()
        .filter_map(|&entity| SmallMapUnit::observe(world, board, entity, distances))
        .collect::<Vec<_>>();
    // 行動済みでも契約は有効。正規の占領所属と未所有の合法目標を両方確認する。
    // 通常攻撃Squadがたまたま指す敵施設を、将来の砲座とは見なさない。
    let tactical = &board.tactical;
    let capture_sites = tactical
        .action_targets
        .units()
        .iter()
        .filter_map(|unit| {
            if unit.faction != tactical.player_id
                || unit.is_transporting
                || tactical.squads.mission(unit.entity) != Some(&MissionType::Capture)
            {
                return None;
            }
            let state = UnitState::observe(world, tactical, unit.entity)?;
            if state.hp == 0 {
                return None;
            }
            let target = crate::ai::v4::capital_route::tactical::capital_route_tactical_target(
                world,
                tactical.player_id,
                unit.entity,
            )
            .flatten();
            small_map_capture_target(world, board, &state, target)
        })
        .collect();
    let holders = units
        .iter()
        .filter(|unit| is_line_holder(world, board, unit, LineHoldMode::Partial))
        .map(|unit| EligibleHolder {
            state: &unit.state,
            stops: &unit.candidate_tiles,
        })
        .collect::<Vec<_>>();
    planner::plan(world, &board.tactical, &holders, &capture_sites)
        .first()
        .copied()
}

/// 隣接攻撃と占領ができる健全な地上兵に限定し、任務の差し替え可否は保持モードで決める。
fn is_line_holder(
    world: &World,
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    mode: LineHoldMode,
) -> bool {
    let state = &unit.state;
    // 緊急迎撃は占領不可兵に限られ、首都防衛もこの戦術の呼び出し前に解決する。
    let campaign = unit.campaign.as_ref().map(|context| {
        (
            &context.mission_type,
            context.departure_authorized
                && campaign_position_is_on_target_island(world, context, state.position),
        )
    });
    state.stats.can_capture
        && state.stats.min_range <= 1
        && state.stats.max_range >= 1
        && !state.combat_ineffective
        && mode.allows_mission(
            board.tactical.squads.mission(state.entity),
            campaign,
            unit.route.tactical_target.flatten().is_some()
                || unit.route.advance_destination.is_some(),
            unit.is_capture_mission || unit.has_capture_target,
        )
        && !matches!(
            state.stats.movement_type,
            crate::resources::MovementType::Air | crate::resources::MovementType::Ship
        )
        && !crate::ai::v4::capital_route::tactical::capital_route_is_recovering(
            world,
            board.tactical.player_id,
            state.entity,
        )
}

/// 小規模マップの行動選択で全unitが共有する盤面の事実。
struct SmallMapBoard {
    tactical: TacticalBoard,
    is_v4: bool,
    production_block_trial: bool,
    /// 小規模マップの作戦プロファイル（盤面サイズとは別に、戦略層が採用したもの）。
    uses_small_map_profile: bool,
    own_production: OwnProductionSites,
    /// 試験: 上の敵を優先攻撃する非生産前線都市（強制保持＋スイッチ時のみ非空）。
    recapture_cities: HashSet<GridPosition>,
    /// 敵生産口の占位。V4限定（V3は対戦評価の基準線として挙動を不変に保つ）。
    blockade: Option<EnemyProductionBlockade>,
    /// 修理滞在中（HP100未満）のunitの位置。回復拠点の空きを判定する。
    repairing_units: Vec<(GridPosition, Entity)>,
}

impl SmallMapBoard {
    fn observe(world: &World, tactical: TacticalBoard) -> Self {
        let player_id = tactical.player_id;
        let ai_version = crate::ai::resolve_player_ai_version(world, player_id);
        let funds = world
            .get_resource::<crate::resources::Players>()
            .and_then(|players| players.0.iter().find(|p| p.id == player_id))
            .map_or(0, |p| p.funds);
        let own_production = OwnProductionSites::observe(&tactical, funds);
        let recapture_cities = forced_recapture_cities(world, &tactical, &own_production);
        let blockade = ai_version
            .uses_enemy_production_blockade()
            .then(|| EnemyProductionBlockade::observe(&tactical));
        let repairing_units = world
            .iter_entities()
            .filter_map(|entity| {
                let position = *entity.get::<GridPosition>()?;
                entity
                    .get::<Health>()
                    .is_some_and(|health| health.current < 100)
                    .then_some((position, entity.id()))
            })
            .collect();
        Self {
            is_v4: ai_version == crate::ai::ai_version::AiVersion::V4,
            production_block_trial: ai_version == crate::ai::ai_version::AiVersion::V4
                && std::env::var_os("OPENWARS_PRODUCTION_BLOCK_TRIAL").as_deref()
                    == Some(std::ffi::OsStr::new("1")),
            uses_small_map_profile: current_profile(world, player_id)
                == V4StrategyProfile::SmallMapExpansion,
            tactical,
            own_production,
            recapture_cities,
            blockade,
            repairing_units,
        }
    }

    /// 小規模マップでは前線から1ターン以上離れる後退を戦線崩壊とみなす（合流判断に使う）。
    fn is_small_map(&self) -> bool {
        let map = &self.tactical.map;
        self.uses_small_map_profile || map.width * map.height <= 200
    }
}

/// 自軍首都の生産可能範囲にある自軍生産施設。
struct OwnProductionSites {
    positions: HashSet<(usize, usize)>,
    /// 全生産施設が自軍unitで埋まっている。各行動後に再評価されるこの契約で1体を押し出し、
    /// 最低1つの後続生産口を回復する（1つでも空きがあれば通常の戦術行動を優先する）。
    gridlocked: bool,
    /// 今ターンに空いている生産枠数（自軍生産施設のうち未占有のもの）。
    free_slots: usize,
    /// 今ターンに購入し得る最大ユニット数（所持金 ÷ 最安ユニット価格）。
    /// 「枠が足りないか」を固定の定数ではなく、資金と価格の需給で判定するための値。
    demand_slots: usize,
}

/// ユニットの最安コスト。全兵種の下限であり、資金から購入可能な最大頭数を導く。
const CHEAPEST_UNIT_COST: u32 = 1_000;

impl OwnProductionSites {
    fn observe(board: &TacticalBoard, funds: u32) -> Self {
        let capitals = board
            .properties
            .iter()
            .filter(|(_, terrain, owner)| {
                *owner == Some(board.player_id) && *terrain == Terrain::Capital
            })
            .map(|(position, ..)| *position)
            .collect::<Vec<_>>();
        let positions = board
            .properties
            .iter()
            .filter(|(position, terrain, owner)| {
                *owner == Some(board.player_id)
                    && board.registry.is_production_facility(terrain.as_str())
                    && is_within_production_range(
                        &capitals,
                        position.x,
                        position.y,
                        board.map.topology,
                    )
            })
            .map(|(position, ..)| (position.x, position.y))
            .collect::<HashSet<_>>();
        let free_slots = positions
            .iter()
            .filter(|position| !board.occupancy.contains_key(position))
            .count();
        let gridlocked = !positions.is_empty() && free_slots == 0;
        Self {
            positions,
            gridlocked,
            free_slots,
            demand_slots: (funds / CHEAPEST_UNIT_COST) as usize,
        }
    }

    fn contains(&self, tile: (usize, usize)) -> bool {
        self.positions.contains(&tile)
    }

    /// 今手番の資金で買える頭数に対して生産枠が不足しているか。
    /// 固定の枠数ではなく、資金と単価の需給だけで判定する。
    fn is_supply_short(&self) -> bool {
        self.free_slots < self.demand_slots
    }
}

/// 首都ルートDAGからこのunitへの指令。
struct RouteOrders {
    /// `Some(None)` はDAGのControl地域に入ったことを表す（地域外の古いSquad目標へ戻さない）。
    tactical_target: Option<Option<GridPosition>>,
    /// DAGセル列上で、この手番に到達できる次の前進先。
    advance_destination: Option<GridPosition>,
}

/// 専任占領役の割当物件と、現在位置からのターン距離。
#[derive(Clone, Copy)]
struct CaptureContract {
    target: GridPosition,
    turns_before: TurnDistance,
}

/// 行動選択で使う1体のunitの事実（小規模マップの契約を含む）。
struct SmallMapUnit {
    state: UnitState,
    /// 生産計画が契約した初撃の標的。
    deployment_target: Option<Entity>,
    campaign: Option<CampaignActionContext>,
    route: RouteOrders,
    squad_target: Option<GridPosition>,
    /// 一般の敵を探す攻勢任務（占領任務は含まない）。
    has_offensive_mission: bool,
    is_capture_mission: bool,
    capture_contract: Option<CaptureContract>,
    /// 味方の退去後にも、通常と同じ移動候補制約を再利用するための事実。
    has_capture_target: bool,
    is_solo: bool,
    starts_on_own_production: bool,
    candidate_tiles: Vec<(usize, usize)>,
    /// 現在位置以外に、自軍生産口ではないWait先がある。
    has_non_production_wait_destination: bool,
    /// 自軍生産口ではないセルで手番を終えられる（現在位置を含む）。
    can_end_turn_off_production: bool,
    /// 間接部隊が追随すべき前衛の後方射点。
    support_destination: Option<GridPosition>,
    /// 戦闘不能unitが向かえる回復拠点（修理滞在中の味方がいる拠点を除く）。
    repair_sites: Vec<GridPosition>,
    /// 強制保持（または砲座配置）の行軍中。移動途中の占領に横取りされない。
    is_forced_hold: bool,
}

impl SmallMapUnit {
    fn restrict_to_hold(&mut self, target: GridPosition) {
        // 移動・待機・移動後攻撃の入口をひとつの停止位置に絞る。
        self.candidate_tiles
            .retain(|&(x, y)| x == target.x && y == target.y);
        self.retarget_to_hold(target);
    }

    /// 強制保持の上限実験: 保持マスに届くならそこで止まり、届かなければ保持マスまでの
    /// ターン距離が最小の移動先だけを残す（移動後の攻撃は残した移動先から行える）。
    /// 自軍生産口での待機は既存規則で禁止されるため、他に候補があれば生産口を除く。
    fn march_to_hold(
        &mut self,
        board: &SmallMapBoard,
        target: GridPosition,
        distances: &mut ApproachDistances,
    ) {
        // 試験で選んだ兵は占領・進軍の戦術目標を保持目標へ差し替える。
        self.is_forced_hold = true;
        self.route = RouteOrders {
            tactical_target: None,
            advance_destination: None,
        };
        self.is_capture_mission = false;
        self.capture_contract = None;
        self.has_capture_target = false;
        let tactical = &board.tactical;
        let state = &self.state;
        let origin = (state.position.x, state.position.y);
        self.candidate_tiles = calculate_reachable_tiles(
            &tactical.map,
            &tactical.occupancy,
            origin,
            state.stats.movement_type,
            state.stats.max_movement,
            state.fuel,
            tactical.player_id,
            state.stats.unit_type,
            &tactical.registry,
        )
        .into_iter()
        .filter(|tile| *tile == origin || !tactical.occupancy.contains_key(tile))
        .collect();
        (
            self.has_non_production_wait_destination,
            self.can_end_turn_off_production,
        ) = production_exits(board, state, &self.candidate_tiles);
        if self.candidate_tiles.contains(&(target.x, target.y)) {
            self.restrict_to_hold(target);
            return;
        }
        let off_production = self
            .candidate_tiles
            .iter()
            .copied()
            .filter(|&tile| !board.own_production.contains(tile))
            .collect::<Vec<_>>();
        let tiles = if off_production.is_empty() {
            std::mem::take(&mut self.candidate_tiles)
        } else {
            off_production
        };
        let keyed = tiles
            .into_iter()
            .map(|(x, y)| {
                let distance = distances.turns(&self.state.stats, GridPosition { x, y }, target, 0);
                ((distance.turns, distance.used_mp), (x, y))
            })
            .collect::<Vec<_>>();
        let best = keyed.iter().map(|(key, _)| *key).min();
        self.candidate_tiles = keyed
            .into_iter()
            .filter(|(key, _)| Some(*key) == best)
            .map(|(_, tile)| tile)
            .collect();
        self.retarget_to_hold(target);
    }

    /// 試験用の砲座では生産枠を空ける通常の待機制約よりも布陣を優先する。
    /// ただしエンジンが待機を許す移動先だけを使う。
    fn wait_for_forced_battery(&self, board: &TacticalBoard) -> Option<(Rank, AiCommand)> {
        self.candidate_tiles.iter().find_map(|&(x, y)| {
            let target_pos = GridPosition { x, y };
            board
                .action_targets
                .action_targets_at(
                    self.state.entity,
                    &self.state.stats,
                    target_pos,
                    target_pos != self.state.position,
                )
                .can_wait
                .then_some(((ActionPriority::Normal, 0), AiCommand::Wait { target_pos }))
        })
    }

    /// 局地戦術の目標を保持マスへ差し替える。所属や出発許可は変えない。
    fn retarget_to_hold(&mut self, target: GridPosition) {
        self.squad_target = Some(target);
        self.deployment_target = None;
        self.has_offensive_mission = false;
        if let Some(campaign) = &mut self.campaign {
            // 局地戦術の敵追跡だけを差し替える。所属や出発許可は変えない。
            campaign.target = Some(target);
        }
    }

    /// 必要な状態が欠けているunit、またはcampaign所属に具体Squadが無くDAGの実行区間も
    /// 持たないunit（作戦再構築へ戻す）は、この手番の候補にしない。
    fn observe(
        world: &World,
        board: &SmallMapBoard,
        entity: Entity,
        distances: &mut ApproachDistances,
    ) -> Option<Self> {
        let tactical = &board.tactical;
        let player_id = tactical.player_id;
        let state = UnitState::observe(world, tactical, entity)?;
        let deployment_target = world
            .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
            .and_then(|registry| registry.attack_target(entity));
        let route_tactical_target =
            crate::ai::v4::capital_route::tactical::capital_route_tactical_target(
                world, player_id, entity,
            );
        // 回復中のDAG Entityは、所属をDAGに残したまま通常の回復探索を使う
        // （古いSquad目標を渡すと、修理ではなく横の拠点へ戻る）。
        let recovering = crate::ai::v4::capital_route::tactical::capital_route_is_recovering(
            world, player_id, entity,
        );
        let campaign =
            match campaign_action_context(world, player_id, entity, &tactical.squads.manager) {
                Ok(context) => context,
                // DAGの実行区間を持つEntityは、DAG自身が一意の行動先を持つので落とさない。
                Err(()) if route_tactical_target.is_some() => None,
                Err(()) => return None,
            };
        let small_map_capture_target =
            small_map_capture_target(world, board, &state, route_tactical_target.flatten());

        // 護衛を待つ占領役は、現在位置での攻撃・占領・Waitだけを残し、単独前進しない。
        let waits_for_escort = small_map_capture_target.is_none()
            && campaign.as_ref().is_some_and(|context| {
                context.mission_type == MissionType::Capture && !context.departure_authorized
            });
        let reachable = if waits_for_escort {
            std::iter::once((state.position.x, state.position.y)).collect()
        } else {
            calculate_reachable_tiles(
                &tactical.map,
                &tactical.occupancy,
                (state.position.x, state.position.y),
                state.stats.movement_type,
                state.stats.max_movement,
                state.fuel,
                player_id,
                state.stats.unit_type,
                &tactical.registry,
            )
        };
        let route = RouteOrders {
            tactical_target: route_tactical_target,
            advance_destination:
                crate::ai::v4::capital_route::tactical::capital_route_advance_destination(
                    world, player_id, entity, &reachable,
                ),
        };
        let starts_on_own_production = board
            .own_production
            .contains((state.position.x, state.position.y));
        let candidate_tiles = candidate_tiles(
            world,
            board,
            &state,
            &route,
            small_map_capture_target.is_some(),
            starts_on_own_production,
            deployment_target,
            &reachable,
        );
        let (has_non_production_wait_destination, can_end_turn_off_production) =
            production_exits(board, &state, &candidate_tiles);

        let squad_target = if recovering {
            None
        } else {
            route_tactical_target
                .flatten()
                .or(tactical.squads.target(entity))
        };
        let squad_mission = tactical.squads.mission(entity);
        let is_capture_mission = campaign
            .as_ref()
            .is_some_and(|context| context.mission_type == MissionType::Capture)
            || squad_mission == Some(&MissionType::Capture);
        // Captureは面制圧パッケージの後続工程であり、一般の敵を探す攻勢任務ではない。
        // 目標物件を直接塞ぐ敵だけは、攻撃の優先度で戦略標的として扱う。
        let has_offensive_mission = !recovering
            && !is_capture_mission
            && (route_tactical_target.is_some()
                || squad_mission.is_some_and(|mission| {
                    matches!(
                        mission,
                        MissionType::Attack | MissionType::Capture | MissionType::Transport
                    )
                }));
        let capture_contract = is_capture_mission
            .then_some(squad_target)
            .flatten()
            .map(|target| CaptureContract {
                target,
                turns_before: distances.turns(&state.stats, state.position, target, 0),
            });
        let is_solo = recovering
            || (tactical.squads.is_solo_fallback(entity) && route_tactical_target.is_none())
            || squad_target.is_none();
        let support_destination =
            (board.is_v4 && state.stats.min_range > 1 && !state.combat_ineffective)
                .then(|| {
                    // すでに前線を射程に収める有効な補給砲座都市に布陣している間は、
                    // 補給と地形防御30%を捨ててまで非補給タイルへ追随前進しない。
                    if is_active_supply_battery_at(tactical, &state, state.position) {
                        return None;
                    }
                    // 現在位置で既に誰かの敵を射程に収めている、または自軍生産施設から出る際に
                    // 1手で有効な補給砲座都市へ入れるなら、前方の道路への追随移動で上書きしない。
                    let can_reach_supply_battery = candidate_tiles.iter().any(|&(x, y)| {
                        let pos = GridPosition { x, y };
                        pos != state.position
                            && !tactical.occupancy.contains_key(&(x, y))
                            && is_active_supply_battery_at(tactical, &state, pos)
                    });
                    if can_reach_supply_battery {
                        return None;
                    }
                    coordinated_support_destination(
                        world,
                        player_id,
                        entity,
                        &state.stats,
                        state.position,
                        squad_target,
                        &candidate_tiles,
                        &tactical.occupancy,
                        &tactical.squads.manager,
                        &tactical.action_targets,
                        &tactical.map,
                        &tactical.registry,
                        &tactical.damage_chart,
                    )
                })
                .flatten();
        let repair_sites = if state.combat_ineffective {
            // 健全な味方なら移動して道を譲れるが、既に他の損耗unitが修理滞在中の拠点は
            // 回復を受け入れられない。
            tactical
                .own_repair_sites(&state.stats)
                .filter(|site| {
                    !board
                        .repairing_units
                        .iter()
                        .any(|(position, other)| position == site && *other != entity)
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(Self {
            state,
            deployment_target,
            campaign,
            route,
            squad_target,
            has_offensive_mission,
            is_capture_mission,
            capture_contract,
            has_capture_target: small_map_capture_target.is_some(),
            is_solo,
            starts_on_own_production,
            candidate_tiles,
            has_non_production_wait_destination,
            can_end_turn_off_production,
            support_destination,
            repair_sites,
            is_forced_hold: false,
        })
    }
}

/// DAG再束縛前でも、RoadmapがCapture Squadへ渡した未所有物件を行動契約として有効にする。
/// Squad目標を捨てて集結待ちへ戻すと、複数の占領レーンを同時に開く小規模マップの初動が失われる。
fn small_map_capture_target(
    world: &World,
    board: &SmallMapBoard,
    unit: &UnitState,
    route_target: Option<GridPosition>,
) -> Option<GridPosition> {
    if !board.uses_small_map_profile || !unit.stats.can_capture {
        return None;
    }
    let player_id = board.tactical.player_id;
    route_target
        .or(board.tactical.squads.target(unit.entity))
        .filter(|target| {
            has_capturable_property_at(world, *target, player_id)
                && crate::ai::v4::capital_route::tactical::capital_route_allows_capture_at(
                    world,
                    player_id,
                    unit.entity,
                    *target,
                )
        })
}

/// 移動先候補（評価順）。同点は先に評価した候補を残すため、順序も契約の一部である。
#[allow(clippy::too_many_arguments)]
fn candidate_tiles(
    world: &World,
    board: &SmallMapBoard,
    unit: &UnitState,
    route: &RouteOrders,
    has_capture_target: bool,
    starts_on_own_production: bool,
    deployment_target: Option<Entity>,
    reachable: &BTreeSet<(usize, usize)>,
) -> Vec<(usize, usize)> {
    let tactical = &board.tactical;
    let origin = (unit.position.x, unit.position.y);
    let mut tiles = if has_capture_target {
        reachable.iter().copied().collect::<Vec<_>>()
    } else if let Some(destination) = route.advance_destination {
        // 進軍役はDAGセル列の次候補と、その場の戦闘・占領だけを比較する。横方向への
        // 通常探索を残すと、橋へ近づくscoreだけで経路から外れる。
        let destination = (destination.x, destination.y);
        let destination_blocked =
            destination == origin || tactical.occupancy.contains_key(&destination);
        if starts_on_own_production && destination_blocked {
            // 工場上でDAGの次マスが塞がっている場合、一本道に縛られて工場上で待機すると
            // 後続の生産を永久に封鎖する。周囲の非生産マスを加えて工場から脱出させる。
            let mut tiles = vec![origin];
            if reachable.contains(&destination) {
                tiles.push(destination);
            }
            tiles.extend(
                reachable
                    .iter()
                    .copied()
                    .filter(|tile| !board.own_production.contains(*tile)),
            );
            tiles.sort_unstable();
            tiles.dedup();
            tiles
        } else {
            [origin, destination]
                .into_iter()
                .filter(|tile| reachable.contains(tile))
                .collect()
        }
    } else {
        reachable
            .iter()
            .copied()
            .filter(|(x, y)| {
                crate::ai::v4::capital_route::tactical::capital_route_allows_tactical_position(
                    world,
                    tactical.player_id,
                    unit.entity,
                    GridPosition { x: *x, y: *y },
                )
                .unwrap_or(true)
            })
            .collect()
    };
    if let Some(planned_target) = deployment_target {
        // 生産計画が具体的な敵Entityとの初撃を契約した場合、DAGの一本道だけで候補を
        // 制限しない。この手番に射撃可能な全セルを加え、複数の快速直接戦闘unitが同じ入口へ
        // 直列化せず、別方向から同じblockerへ集中攻撃できるようにする。
        tiles.extend(reachable.iter().copied().filter(|&(x, y)| {
            tactical
                .action_targets
                .action_targets_at(
                    unit.entity,
                    &unit.stats,
                    GridPosition { x, y },
                    (x, y) != origin,
                )
                .attackable_targets
                .contains(&planned_target)
        }));
        tiles.sort_unstable();
        tiles.dedup();
    }
    if !unit.stats.can_capture {
        // 非占領unitが、友軍の占領予定の未所有物件を踏んで占領を物理的に塞ぐのを防ぐ。
        // 敵の生産マスも例外にしない。占領は補充を止めたうえで収入と生産力を自軍へ移すため
        // 占位の上位互換であり、占位は誰も占領しに行かないマスに対する代替手段である。
        let non_reserved = tiles
            .iter()
            .copied()
            .filter(|&(x, y)| {
                let position = GridPosition { x, y };
                let is_unowned = tactical.properties.iter().any(|(site, _, owner)| {
                    *site == position && *owner != Some(tactical.player_id)
                });
                !is_unowned
                    || !crate::ai::v4::capital_route::tactical::is_capture_target_reserved(
                        world,
                        tactical.player_id,
                        position,
                    )
            })
            .collect::<Vec<_>>();
        if !non_reserved.is_empty() {
            tiles = non_reserved;
        }
    }
    tiles
}

/// 自軍生産口の外で手番を終えられるか: (現在位置以外の非生産Wait先がある,
/// 現在位置を含めて非生産セルで手番を終えられる)。
fn production_exits(
    board: &SmallMapBoard,
    unit: &UnitState,
    tiles: &[(usize, usize)],
) -> (bool, bool) {
    let origin = (unit.position.x, unit.position.y);
    let can_wait_at = |(x, y): (usize, usize)| {
        !board.own_production.contains((x, y))
            && board
                .tactical
                .action_targets
                .action_targets_at(
                    unit.entity,
                    &unit.stats,
                    GridPosition { x, y },
                    (x, y) != origin,
                )
                .can_wait
    };
    let has_non_production_wait_destination = tiles
        .iter()
        .any(|&tile| tile != origin && can_wait_at(tile));
    let can_end_turn_off_production = tiles.iter().any(|&tile| can_wait_at(tile));
    (
        has_non_production_wait_destination,
        can_end_turn_off_production,
    )
}

/// 1つの移動先の事実。
struct TileCandidate {
    position: GridPosition,
    is_stationary: bool,
    actions: TacticalActionTargets,
    /// 移動先そのものの価値（位置取り・目標への接近・占位）。各行動のscoreの土台になる。
    base_score: i32,
    /// 全生産口が閉塞しているとき、工場上の健全なunitが工場外へ出る移動。
    relieves_gridlock: bool,
    /// 占領契約の割当物件へ、実経路上で近づく（または到達する）移動。
    advances_capture: bool,
    /// DAGセル列上の前進（現在セルは前進ではない）。
    is_route_advance: bool,
    blockade: BlockadeTile,
    /// 最寄りの空いた回復拠点までのターン数（戦闘不能unitのみ）。
    nearest_repair_turns: Option<u32>,
}

impl TileCandidate {
    fn observe(
        world: &World,
        board: &SmallMapBoard,
        unit: &SmallMapUnit,
        position: GridPosition,
        distances: &mut ApproachDistances,
    ) -> Self {
        let tactical = &board.tactical;
        let state = &unit.state;
        let is_stationary = position == state.position;
        let actions = tactical.action_targets.action_targets_at(
            state.entity,
            &state.stats,
            position,
            !is_stationary,
        );
        let relieves_gridlock = board.own_production.gridlocked
            && unit.starts_on_own_production
            && !state.combat_ineffective
            && !board.own_production.contains((position.x, position.y));
        let advances_capture = unit.capture_contract.is_some_and(|contract| {
            position == contract.target
                || distances.turns(&state.stats, position, contract.target, 0)
                    < contract.turns_before
        });
        let defense_bonus = terrain_defense_bonus(tactical, position);
        let blockade = board
            .blockade
            .as_ref()
            .map(|blockade| blockade.assess(tactical, state, position, defense_bonus))
            .unwrap_or_default();
        let mut tile = Self {
            position,
            is_stationary,
            actions,
            base_score: 0,
            relieves_gridlock,
            advances_capture,
            is_route_advance: !is_stationary && unit.route.advance_destination == Some(position),
            blockade,
            nearest_repair_turns: None,
        };
        tile.base_score = tile.score(world, board, unit, defense_bonus, distances);
        tile
    }

    /// 移動先そのものの価値。小規模マップでは、戦闘不能unitは単独行動か否かによらず
    /// 空いた回復拠点へ向かう。
    fn score(
        &mut self,
        world: &World,
        board: &SmallMapBoard,
        unit: &SmallMapUnit,
        defense_bonus: u32,
        distances: &mut ApproachDistances,
    ) -> i32 {
        let tactical = &board.tactical;
        let state = &unit.state;
        let position = self.position;
        let mut score = defense_bonus as i32 * 10;
        if self.blockade.holds_site {
            score += self.blockade.site_value as i32;
        }
        if tactical.uses_v3_tactics {
            score += v3_positioning_score(tactical, state, position, defense_bonus);
            score += frontline_threat_terrain_score(tactical, state, position, defense_bonus);
        }
        let on_active_supply_battery =
            board.is_v4 && is_active_supply_battery_at(tactical, state, position);
        if on_active_supply_battery {
            score += SUPPLY_BATTERY_HOLD_BONUS;
        }
        // 間接部隊の支援射点: 前衛の目標を射程に収める位置へ加点し、射程外への前進を抑える。
        // 既に前線を射程に収める有効な補給砲座都市にいる場合は、砲座自体も支援射点と同等に扱う。
        if board.is_v4
            && state.stats.min_range > 1
            && let Some(target) = unit.squad_target
        {
            let distance = tactical
                .map
                .distance(target.x, target.y, position.x, position.y);
            if (state.stats.min_range..=state.stats.max_range).contains(&distance)
                || on_active_supply_battery
            {
                score += 5000;
            }
        }
        if !unit.is_solo
            && let Some(target) = unit.squad_target
        {
            // 前線を射程に収める有効な補給砲座都市にいる（またはこのターンに補給砲座都市へ入る）間接部隊は、
            // 奥の目標へ距離を縮めるだけの理由で非補給タイル（道路や平地）を優先しない。
            let can_use_supply_battery = board.is_v4
                && state.stats.min_range > 1
                && (on_active_supply_battery
                    || is_active_supply_battery_at(tactical, state, state.position));
            let approach_pos = if can_use_supply_battery && !on_active_supply_battery {
                state.position
            } else {
                position
            };
            score += squad_target_approach_score(state, approach_pos, target, distances);
        }
        if state.combat_ineffective {
            let nearest =
                distances.nearest(&state.stats, position, unit.repair_sites.iter().copied(), 0);
            self.nearest_repair_turns = nearest.map(|(turns, _)| turns.turns);
            score += nearest_approach_bonus(nearest, state.stats.max_movement);
        } else if unit.is_solo && !state.stats.can_capture {
            score += solo_enemy_approach_score(tactical, state, position, distances);
        }
        score += empty_transport_return_score(world, tactical, state, position, distances);
        score += stranded_infantry_score(world, tactical, state, position, distances);
        score += capture_or_hunt_score(world, tactical, state, unit.is_solo, position, distances);
        score
    }

    /// 全生産口の閉塞を解く移動は、どの行動でも最優先にする。
    fn priority_or_relief(&self, priority: ActionPriority) -> ActionPriority {
        if self.relieves_gridlock {
            ActionPriority::ProductionSiteRelief
        } else {
            priority
        }
    }
}

/// 【診断】攻撃候補の採否を記録する（`OPENWARS_SMALL_MAP_TRACE_TURN` の手番だけ）。
#[derive(Default)]
struct AttackLog {
    enabled: bool,
    lines: Vec<String>,
}

impl AttackLog {
    fn push(&mut self, line: impl FnOnce() -> String) {
        if self.enabled {
            self.lines.push(line());
        }
    }
}

/// 【診断】トレース対象の手番・プレイヤーか。
/// `OPENWARS_SMALL_MAP_TRACE_TURN` は手番番号、または全手番を表す `all`。
fn small_map_trace_enabled(world: &World, player: PlayerId) -> bool {
    let Ok(value) = std::env::var("OPENWARS_SMALL_MAP_TRACE_TURN") else {
        return false;
    };
    let turn = if value == "all" {
        None
    } else {
        let Ok(turn) = value.parse::<u32>() else {
            return false;
        };
        Some(turn)
    };
    let player_ok = std::env::var("OPENWARS_SMALL_MAP_TRACE_PLAYER")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .is_none_or(|target| target == player.0);
    player_ok
        && turn.is_none_or(|turn| {
            world
                .get_resource::<crate::resources::MatchState>()
                .is_some_and(|state| state.current_turn_number.0 == turn)
        })
}

/// 1体のunitの全移動先・全行動のうち最良の (優先度, score) と命令。`log` には攻撃候補の採否を記録する。
fn best_action_for_unit_logged(
    world: &World,
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    distances: &mut ApproachDistances,
    log: &mut AttackLog,
) -> BestAction {
    let tactical = &board.tactical;
    let state = &unit.state;
    let mut best = BestAction::default();
    // 現在位置から撃てる最良の攻撃。間接unitは移動すると同一手番で射撃できないため、
    // 移動候補との比較に使う。
    let mut best_stationary_attack: Option<(Rank, Entity)> = None;
    for &(x, y) in &unit.candidate_tiles {
        let position = GridPosition { x, y };
        if !air_move_preserves_return_fuel(
            &tactical.map,
            &tactical.properties,
            tactical.player_id,
            &state.stats,
            state.position,
            position,
            state.fuel,
        ) {
            continue;
        }
        let tile = TileCandidate::observe(world, board, unit, position, distances);
        consider_capture(world, board, unit, &tile, &mut best);
        consider_attacks(
            world,
            board,
            unit,
            &tile,
            &mut best,
            &mut best_stationary_attack,
            log,
        );
        consider_wait(board, unit, &tile, distances, &mut best);
        consider_merges(world, board, unit, &tile, &mut best);
    }
    prefer_fire_in_place(board, unit, best_stationary_attack, &mut best);
    best
}

/// 占領。DAGが許す物件で、campaign所属unitは占領を許す任務で作戦島の上にいる場合だけ。
fn consider_capture(
    world: &World,
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    tile: &TileCandidate,
    best: &mut BestAction,
) {
    if unit.is_forced_hold
        || !tile.actions.can_capture
        || !crate::ai::v4::capital_route::tactical::capital_route_allows_capture_at(
            world,
            board.tactical.player_id,
            unit.state.entity,
            tile.position,
        )
    {
        return;
    }
    let outside_campaign = unit.campaign.as_ref().is_some_and(|context| {
        !campaign_mission_allows_capture(&context.mission_type)
            || !campaign_position_is_on_target_island(world, context, tile.position)
    });
    if outside_campaign {
        return;
    }
    let priority = tile.priority_or_relief(if tile.advances_capture {
        ActionPriority::CaptureExecution
    } else if tile.is_route_advance {
        ActionPriority::RouteAdvance
    } else {
        ActionPriority::Normal
    });
    best.offer((priority, tile.base_score + CAPTURE_SCORE), || {
        AiCommand::Capture {
            target_pos: tile.position,
        }
    });
}

/// 攻撃。工場上の砲撃も合法な候補として残し、射撃と工場の退去を比較できるようにする。
fn consider_attacks(
    world: &World,
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    tile: &TileCandidate,
    best: &mut BestAction,
    best_stationary_attack: &mut Option<(Rank, Entity)>,
    log: &mut AttackLog,
) {
    for target in tile.actions.attackable_targets.iter().copied() {
        if !attack_serves_campaign(
            world,
            unit.campaign.as_ref(),
            unit.deployment_target,
            target,
        ) {
            log.push(|| format!("{target:?}@{:?}:作戦外", (tile.position.x, tile.position.y)));
            continue;
        }
        let Some(exchange) = crate::ai::pruning::evaluate_attack_exchange(
            world,
            unit.state.entity,
            target,
            tile.position,
            &board.tactical.damage_chart,
        ) else {
            continue;
        };
        let Some(attack) = assess_attack(world, &board.tactical, &unit.state, target, &exchange)
        else {
            continue;
        };
        // 今ターンで撃破し、同時解決の反撃を受けても生存できる攻撃。
        let is_survivable_lethal_strike = attack.expected_damage >= attack.target_hp
            && exchange.expected_counter_damage < unit.state.hp;
        // 攻撃後に残存する周囲の敵（今回の攻撃で削った・撃破した標的自身のHP減算を反映済み）から
        // 次ターンに受ける期待損失額を計算する。
        // 対空戦車や戦車・自走砲を直接叩き潰す攻撃は次ターンの反撃脅威が消える（または高価値与ダメで黒字になる）一方、
        // 無傷の対空戦車・戦闘機・ミサイルの射程内で安価な歩兵を叩いて高価な爆撃機等を失う赤字攻撃を抑止する。
        let post_counter_hp = unit
            .state
            .hp
            .saturating_sub(exchange.expected_counter_damage);
        let attack_def_bonus = terrain_defense_bonus(&board.tactical, tile.position);
        let post_attack_loss_value = if board.is_v4 && unit.state.stats.min_range <= 1 {
            crate::ai::threat::post_attack_expected_loss_value(
                &board.tactical.map,
                (tile.position.x, tile.position.y),
                unit.state.stats.unit_type,
                unit.state.stats.cost,
                post_counter_hp,
                attack_def_bonus,
                attack.target_position,
                attack.expected_damage,
                &board.tactical.enemies,
                &board.tactical.damage_chart,
                &board.tactical.registry,
            )
        } else {
            0
        };
        if board.is_v4
            && unit.state.stats.movement_type == crate::resources::MovementType::Air
            && post_attack_loss_value > (exchange.damage_value_dealt as i32).saturating_mul(2)
            && post_attack_loss_value - (exchange.damage_value_dealt as i32) >= 4_000
        {
            log.push(|| {
                format!(
                    "{target:?}@{:?}:航空の損失見込み",
                    (tile.position.x, tile.position.y)
                )
            });
            continue;
        }
        let Some(priority) = attack_priority(
            board,
            unit,
            target,
            attack.target_position,
            exchange.is_favorable_matchup(),
            is_survivable_lethal_strike,
        ) else {
            log.push(|| {
                format!(
                    "{target:?}@{:?}:優先度なし(有利={} 任務標的={:?} 攻勢任務={})",
                    (tile.position.x, tile.position.y),
                    exchange.is_favorable_matchup(),
                    unit.deployment_target,
                    unit.has_offensive_mission
                )
            });
            continue;
        };
        let priority = if tile.relieves_gridlock {
            ActionPriority::ProductionSiteRelief
        } else if tile.blockade.holds_site {
            priority.max(ActionPriority::EnemyProductionBlockade)
        } else {
            priority
        };
        let mut attack_score = tile.base_score + attack.score - post_attack_loss_value;
        let on_own_facility = board
            .own_production
            .contains((tile.position.x, tile.position.y));
        if !board.production_block_trial
            && unit.starts_on_own_production
            && !unit.state.combat_ineffective
        {
            // 自軍生産施設から手番を開始した健全ユニットの射撃位置選択:
            // 以下の3条件がすべて満たされたとき、施設外への移動射撃を優先して後続の生産枠を空ける。
            // ① 経済的必要性: 今手番の資金に対して空き枠が不足している（is_supply_short）
            // ② 戦術的生存性: 施設外に出ても反撃で撃破されず、価値交換で大損しない
            // ③ 火力の同等性: 施設外でも有効な攻撃が可能（assess_attack を通過済み）
            let survives_and_favorable = exchange.expected_counter_damage < unit.state.hp
                && !exchange.loses_more_value_than_it_deals();
            if board.own_production.is_supply_short() && survives_and_favorable && !on_own_facility
            {
                // 工場の地形防御差（平地との差約150点）を上回る生産枠解放ボーナスを与え、
                // 施設外に出て撃つ選択を優先する。
                attack_score += 500;
            }
        }
        let slot_cost = if board.production_block_trial
            && on_own_facility
            && unit.can_end_turn_off_production
            // 退去して1枠増やしても使う余地がなければ減点しない。
            && board.own_production.free_slots
                .saturating_sub(usize::from(!unit.starts_on_own_production))
                < board.own_production.demand_slots
        {
            CHEAPEST_UNIT_COST as i32
        } else {
            0
        };
        let combat_gain = (exchange.damage_value_dealt as i32)
            .saturating_sub(exchange.counter_value_received as i32)
            .saturating_sub(post_attack_loss_value);
        let rank = rank_shot_with_production_cost(
            (priority, attack_score),
            combat_gain,
            slot_cost,
            tile.base_score,
        );
        log.push(|| {
            format!(
                "{target:?}@{:?}:{:?}/{}(与{} 反{} 損失見込み{})",
                (tile.position.x, tile.position.y),
                priority,
                attack_score,
                exchange.damage_value_dealt,
                exchange.expected_counter_damage,
                post_attack_loss_value
            )
        });
        if tile.is_stationary && best_stationary_attack.is_none_or(|(best, _)| rank > best) {
            *best_stationary_attack = Some((rank, target));
        }
        best.offer(rank, || AiCommand::Attack {
            target_pos: tile.position,
            target_entity: target,
        });
    }
}

/// 生産枠に見合わない射撃は、攻撃の固定加点・優先度ではなく、
/// その位置の価値と実際の交換利益で退去候補と比較する。
fn rank_shot_with_production_cost(rank: Rank, combat_gain: i32, slot_cost: i32, base: i32) -> Rank {
    if slot_cost == 0 {
        rank
    } else if combat_gain <= slot_cost {
        (
            ActionPriority::Normal,
            base.saturating_add(combat_gain.saturating_sub(slot_cost)),
        )
    } else {
        (rank.0, rank.1.saturating_sub(slot_cost))
    }
}

/// 攻撃の優先度。占領任務では割当物件を直接塞ぐ敵だけが戦略標的になる。
/// 作戦上の必要性がない不利交換はNone。
fn attack_priority(
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    target: Entity,
    target_position: GridPosition,
    favorable_matchup: bool,
    is_survivable_lethal_strike: bool,
) -> Option<ActionPriority> {
    let blocks_capture_target = unit.is_capture_mission
        && unit
            .capture_contract
            .is_some_and(|contract| contract.target == target_position);
    // 試験: 奪われた・敵が乗った前線都市の上の敵は、任務によらず作戦上の標的とする。
    let occupies_lost_city = board.recapture_cities.contains(&target_position);
    let is_strategic_target = occupies_lost_city
        || if unit.is_capture_mission {
            blocks_capture_target
        } else {
            unit.deployment_target
                .map_or(unit.has_offensive_mission, |deployment| {
                    deployment == target
                })
        };
    let has_strategic_mission = occupies_lost_city
        || unit.deployment_target.is_some()
        || unit.has_offensive_mission
        || blocks_capture_target;
    match (is_strategic_target, favorable_matchup) {
        (true, true) => Some(ActionPriority::FavorableStrategicTarget),
        (false, true) if has_strategic_mission => Some(ActionPriority::FavorableLocalTarget),
        // 実行段階の作戦Entityは、他に有利な敵がいない場合だけ不利交換も受容する。
        (true, false) => Some(ActionPriority::StrategicTargetFallback),
        // 攻勢任務・作戦を抱えるEntityは、生存可能な致死攻撃までは消さない
        // （瀕死の隣接敵をWaitで見逃す事故の防止）。
        (false, false) if board.is_v4 && has_strategic_mission && is_survivable_lethal_strike => {
            Some(ActionPriority::Normal)
        }
        (false, false) => None,
        (false, true) => Some(ActionPriority::Normal),
    }
}

/// 待機。自軍生産口を塞ぐ待機と、campaign目標から（経路長で）遠ざかる待機はしない。
fn consider_wait(
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    tile: &TileCandidate,
    distances: &mut ApproachDistances,
    best: &mut BestAction,
) {
    if !tile.actions.can_wait {
        return;
    }
    let tactical = &board.tactical;
    let state = &unit.state;
    let occupies_own_production = board
        .own_production
        .contains((tile.position.x, tile.position.y));
    // 工場外で手番を終えられる消耗unitを、回復点という理由だけで工場へ逆走させない。
    // 既に工場上で損耗しているunitは代替回復点が無い場合もあるため、退去は強制しない。
    let enters_production_for_recovery = state.combat_ineffective
        && occupies_own_production
        && !unit.starts_on_own_production
        && unit.can_end_turn_off_production;
    // 健全なunitが行動終了できる非生産セルを持つなら、工場へ入る／残る必要はない。
    // 減点だけではSquad接近点に負けて翌手番の生産枠を失うため、物理的な排他条件にする。
    let healthy_unit_blocks_production = !state.combat_ineffective
        && occupies_own_production
        && (unit.has_non_production_wait_destination || !unit.starts_on_own_production);
    if enters_production_for_recovery || healthy_unit_blocks_production {
        return;
    }
    let repair_site = repair_site_at(tactical, state, tile.position);
    let wait_recovery = if board.is_v4
        && state.stats.min_range > 1
        && repair_site == Some(false)
        && is_active_supply_battery_at(tactical, state, tile.position)
    {
        // 前線を射程に収める有効な補給砲座都市では、満タンの間接砲も退去ペナルティ(-2000)を受けない。
        wait_recovery_score(state, repair_site).max(0)
    } else {
        wait_recovery_score(state, repair_site)
    };
    let mut score = tile.base_score + wait_recovery;
    let holds_blockade = tile.blockade.holds_site;
    // 占位の価値は base_score で一度計上済みなので、居座りへ重ねて加点しない。
    // 敵生産口から始めた健全なunitが、占位を続けられない敵生産口で待機する場合は、
    // その占位価値の2倍を減点する（移動先が敵生産口でなければ価値は0）。
    if !holds_blockade && tile.blockade.starts_on_site && !state.combat_ineffective {
        score -= tile.blockade.site_value as i32 * 2;
    }
    let moves_away_from_campaign = !unit.is_forced_hold
        && unit.campaign.as_ref().is_some_and(|context| {
            if tile.is_stationary
                || (repair_site.is_some() && state.combat_ineffective)
                || holds_blockade
            {
                return false;
            }
            unit.route
                .tactical_target
                .flatten()
                .or(context.target)
                .is_none_or(|target| {
                    distances
                        .target_progress(&state.stats, state.position, tile.position, target, 0)
                        .moves_away()
                })
        });
    if moves_away_from_campaign {
        return;
    }
    let priority = tile.priority_or_relief(if tile.advances_capture {
        ActionPriority::CaptureAdvance
    } else if holds_blockade {
        ActionPriority::EnemyProductionBlockade
    } else if tile.is_route_advance {
        ActionPriority::RouteAdvance
    } else if unit.support_destination == Some(tile.position) {
        ActionPriority::SupportAdvance
    } else {
        ActionPriority::Normal
    });
    best.offer((priority, score), || AiCommand::Wait {
        target_pos: tile.position,
    });
}

/// 合流。損耗unitが前線で即時合流すべき局面（TacticalMerge）は都市への帰還より優先する。
fn consider_merges(
    world: &World,
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    tile: &TileCandidate,
    best: &mut BestAction,
) {
    for target in tile.actions.mergeable_targets.iter().copied() {
        let Some((merge_score, target_health)) =
            assess_merge(world, &unit.state, unit.campaign.as_ref(), target)
        else {
            continue;
        };
        let priority =
            tile.priority_or_relief(if should_tactical_merge(board, unit, tile, target_health) {
                ActionPriority::TacticalMerge
            } else {
                ActionPriority::Normal
            });
        best.offer((priority, tile.base_score + merge_score), || {
            AiCommand::Merge {
                target_pos: tile.position,
                target_entity: target,
            }
        });
    }
}

/// 前線即時合流の判断基準。合流で戦力が実質的に回復し（自身か相手が損耗）、HPの切り捨てが
/// 少ない（合計110以下）うえで、次のいずれかに当たる場合。
/// - 生存: 敵の攻撃到達圏で低HP（40以下）だが、合流すれば耐えられる（60以上）
/// - ZoC維持: 前線（敵の移動圏内）にあり、退去すると戦線が崩壊する
/// - 帰還困難: 空いた回復拠点が遠い（小規模マップは1ターン、広いマップは2ターンを超える）
fn should_tactical_merge(
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    tile: &TileCandidate,
    target_health: Health,
) -> bool {
    let state = &unit.state;
    let total_hp = state.hp + target_health.current;
    let heals_effectively = state.combat_ineffective || target_health.current < 70;
    if !heals_effectively || total_hp > 110 {
        return false;
    }
    let tactical = &board.tactical;
    let enemy_within = |extra_reach: u32| {
        tactical.enemies.iter().any(|enemy| {
            enemy.hp > 0
                && tactical.map.distance(
                    tile.position.x,
                    tile.position.y,
                    enemy.position.x,
                    enemy.position.y,
                ) <= enemy.max_movement + extra_reach
        })
    };
    let survive_by_merging = enemy_within(1) && state.hp <= 40 && total_hp >= 60;
    let near_threatened_front = enemy_within(0);
    let max_retreat_turns = if board.is_small_map() { 1 } else { 2 };
    let no_fast_repair = tile
        .nearest_repair_turns
        .is_none_or(|turns| turns > max_retreat_turns);
    survive_by_merging || near_threatened_front || no_fast_repair
}

/// 間接unitは移動すると同一手番で射撃できない。現在位置から合法な射撃があるのに前進・待機を
/// 選ぶと射撃機会そのものを失うため、定位置射撃を優先する。ただし自軍生産口の上に立ち、
/// 工場外で手番を終えられるunitには強制しない（射撃と生産口の解放の選択は通常の順位に委ねる）。
fn prefer_fire_in_place(
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    best_stationary_attack: Option<(Rank, Entity)>,
    best: &mut BestAction,
) {
    let state = &unit.state;
    let fire_in_place_blocks_production =
        unit.starts_on_own_production && unit.can_end_turn_off_production;
    let can_relocate_off_factory_to_supply_battery = unit.starts_on_own_production
        && unit.can_end_turn_off_production
        && unit.candidate_tiles.iter().any(|&(x, y)| {
            let pos = GridPosition { x, y };
            pos != state.position
                && !board.own_production.contains((x, y))
                && !board.tactical.occupancy.contains_key(&(x, y))
                && is_active_supply_battery_at(&board.tactical, state, pos)
        });
    if !board.is_v4
        || state.stats.min_range <= 1
        || fire_in_place_blocks_production
        || can_relocate_off_factory_to_supply_battery
    {
        return;
    }
    let Some((rank, target)) = best_stationary_attack else {
        return;
    };
    let fires_in_place = matches!(
        best.command(),
        Some(AiCommand::Attack { target_pos, .. }) if *target_pos == state.position
    );
    if !fires_in_place {
        best.replace(
            rank,
            AiCommand::Attack {
                target_pos: state.position,
                target_entity: target,
            },
        );
    }
}

mod execution_order;
pub(crate) mod squad_executor;

#[cfg(test)]
mod hold_tests {
    use super::*;

    // 生産口を塞ぐ弱い射撃は、施設外へ出る通常行動に優先度だけで勝たない。
    #[test]
    fn production_slot_can_outweigh_a_weak_shot() {
        let wait = (ActionPriority::Normal, 100);
        let shot = (ActionPriority::FavorableStrategicTarget, 2500);
        assert!(rank_shot_with_production_cost(shot, 600, 1000, 100) < wait);
    }

    // 強い射撃は禁止しない。生産枠を失わない射撃の評価も変えない。
    #[test]
    fn production_slot_does_not_forbid_a_valuable_shot() {
        let wait = (ActionPriority::Normal, 100);
        let shot = (ActionPriority::FavorableStrategicTarget, 5000);
        assert!(rank_shot_with_production_cost(shot, 3000, 1000, 100) > wait);
        assert_eq!(rank_shot_with_production_cost(shot, 0, 0, 100), shot);
    }

    #[test]
    fn forced_hold_can_reassign_an_infantry_capture_contract() {
        let capture = MissionType::Capture;
        let campaign = Some((&capture, true));
        assert!(!LineHoldMode::Partial.allows_mission(Some(&capture), campaign, true, true));
        assert!(LineHoldMode::Forced.allows_mission(Some(&capture), campaign, true, true));
        assert!(!LineHoldMode::Forced.allows_mission(
            Some(&MissionType::Defense),
            None,
            false,
            false
        ));
    }

    #[test]
    fn hold_line_can_replace_only_unconstrained_pursuit() {
        let attack = MissionType::Attack;
        let capture = MissionType::Capture;
        assert!(has_only_enemy_pursuit(
            Some(&attack),
            Some((&attack, true)),
            false,
            false
        ));
        assert!(!has_only_enemy_pursuit(
            Some(&attack),
            Some((&attack, false)),
            false,
            false
        ));
        assert!(!has_only_enemy_pursuit(
            Some(&attack),
            Some((&capture, true)),
            false,
            false
        ));
        assert!(!has_only_enemy_pursuit(Some(&attack), None, true, false));
        assert!(!has_only_enemy_pursuit(Some(&attack), None, false, true));
        assert!(!has_only_enemy_pursuit(
            Some(&MissionType::Defense),
            None,
            false,
            false
        ));
    }
}
