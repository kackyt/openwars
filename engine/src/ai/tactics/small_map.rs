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
    BestAction, BestUnitAction, CAPTURE_SCORE, Rank, assess_attack, assess_merge,
    attack_serves_campaign, capture_or_hunt_score, empty_transport_return_score, repair_site_at,
    solo_enemy_approach_score, squad_target_approach_score, stranded_infantry_score,
    terrain_defense_bonus, v3_positioning_score, wait_recovery_score,
};
use super::unit::UnitState;
use crate::ai::command::AiCommand;
use crate::ai::context::tactical::has_capturable_property_at;
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
use crate::collections::HashSet;
use crate::components::{GridPosition, Health, PlayerId};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Terrain, UnitType};
use crate::systems::action::TacticalActionTargets;
use crate::systems::movement::calculate_reachable_tiles;
use crate::systems::production::is_within_production_range;
use bevy_ecs::prelude::*;
use std::collections::BTreeSet;

/// Squad executorから渡されたmember集合（Noneなら全unit）から最良の1手を選び、
/// 評価内訳をEventで通知する。
pub(crate) fn decide_action(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &HashSet<Entity>,
    permitted: Option<&HashSet<Entity>>,
) -> Option<(Entity, AiCommand)> {
    let (movable_units, occupancy) =
        movable_units_and_occupancy(world, player_id, skip_entities, permitted);
    if movable_units.is_empty() {
        return None;
    }
    let manager = world
        .get_resource::<SquadManager>()
        .cloned()
        .unwrap_or_default();
    let squads = SquadRoles::for_small_map(world, player_id, manager);
    let tactical = TacticalBoard::observe(world, player_id, occupancy, squads);
    let board = SmallMapBoard::observe(world, tactical);
    let best = with_turn_distances(world, &board.tactical, |world, distances| {
        let mut best = BestUnitAction::default();
        for entity in movable_units {
            let Some(unit) = SmallMapUnit::observe(world, &board, entity, distances) else {
                continue;
            };
            best.offer(
                entity,
                best_action_for_unit(world, &board, &unit, distances),
            );
        }
        best
    });
    best.choose(world, &board.tactical)
}

/// 小規模マップの行動選択で全unitが共有する盤面の事実。
struct SmallMapBoard {
    tactical: TacticalBoard,
    is_v4: bool,
    /// 小規模マップの作戦プロファイル（盤面サイズとは別に、戦略層が採用したもの）。
    uses_small_map_profile: bool,
    own_production: OwnProductionSites,
    /// 敵生産口の占位。V4限定（V3は対戦評価の基準線として挙動を不変に保つ）。
    blockade: Option<EnemyProductionBlockade>,
    /// 修理滞在中（HP100未満）のunitの位置。回復拠点の空きを判定する。
    repairing_units: Vec<(GridPosition, Entity)>,
}

impl SmallMapBoard {
    fn observe(world: &World, tactical: TacticalBoard) -> Self {
        let player_id = tactical.player_id;
        let ai_version = crate::ai::resolve_player_ai_version(world, player_id);
        let own_production = OwnProductionSites::observe(&tactical);
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
            uses_small_map_profile: current_profile(world, player_id)
                == V4StrategyProfile::SmallMapExpansion,
            tactical,
            own_production,
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
}

impl OwnProductionSites {
    fn observe(board: &TacticalBoard) -> Self {
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
        let gridlocked = !positions.is_empty()
            && positions
                .iter()
                .all(|position| board.occupancy.contains_key(position));
        Self {
            positions,
            gridlocked,
        }
    }

    fn contains(&self, tile: (usize, usize)) -> bool {
        self.positions.contains(&tile)
    }
}

/// 敵生産口の占位（blockade）。自軍の生産口を塞がない知識を攻勢的に反転させ、
/// 敵の補充を物理的に遮断する。
struct EnemyProductionBlockade {
    /// 敵首都の生産可能範囲にある敵所有の生産施設。
    sites: HashSet<GridPosition>,
    /// 敵の補充依存度（分子, 分母）。占位価値の局面係数。
    dependence: (u32, u32),
}

/// 1つの移動先での占位の事実。
#[derive(Default, Clone, Copy)]
struct BlockadeTile {
    starts_on_site: bool,
    /// 移動先が敵生産口なら、その占位価値（それ以外は0）。
    site_value: u32,
    /// 移動先が敵生産口で、かつ占位して生き残れる（戦闘可能で、次ターンの期待被害がHP未満）。
    holds_site: bool,
}

impl EnemyProductionBlockade {
    fn observe(board: &TacticalBoard) -> Self {
        let enemy_capitals = board
            .properties
            .iter()
            .filter_map(|(position, terrain, owner)| match owner {
                Some(owner) if *owner != board.player_id && *terrain == Terrain::Capital => {
                    Some((*owner, *position))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let sites = board
            .properties
            .iter()
            .filter_map(|(position, terrain, owner)| {
                let owner = owner.filter(|owner| *owner != board.player_id)?;
                if !board.registry.is_production_facility(terrain.as_str()) {
                    return None;
                }
                let owner_capitals = enemy_capitals
                    .iter()
                    .filter_map(|(capital_owner, capital)| {
                        (*capital_owner == owner).then_some(*capital)
                    })
                    .collect::<Vec<_>>();
                is_within_production_range(
                    &owner_capitals,
                    position.x,
                    position.y,
                    board.map.topology,
                )
                .then_some(*position)
            })
            .collect::<HashSet<_>>();
        let dependence = enemy_replacement_dependence(sites.len(), board.enemies.len());
        Self { sites, dependence }
    }

    fn assess(
        &self,
        board: &TacticalBoard,
        unit: &UnitState,
        position: GridPosition,
        defense_bonus: u32,
    ) -> BlockadeTile {
        let starts_on_site = self.sites.contains(&unit.position);
        if !self.sites.contains(&position) {
            return BlockadeTile {
                starts_on_site,
                ..BlockadeTile::default()
            };
        }
        // 施設が供給し得る補充の価値を、敵がその補充にどれだけ依存しているかで割り引く。
        let site_value = board
            .properties
            .iter()
            .find(|(site, ..)| *site == position)
            .map(|(_, terrain, _)| production_facility_blockade_value(*terrain, &board.registry))
            .unwrap_or(0)
            .saturating_mul(self.dependence.0)
            / self.dependence.1;
        // 序盤の自殺的突入を抑止するため、戦闘不能unitや次ターン即死圏
        // （期待被damage >= 現在HP）は占位から除外する。
        let incoming_damage = crate::ai::threat::expected_incoming_damage(
            &board.map,
            (position.x, position.y),
            unit.stats.unit_type,
            defense_bonus,
            &board.enemies,
            &board.damage_chart,
        );
        BlockadeTile {
            starts_on_site,
            site_value,
            holds_site: !unit.combat_ineffective && incoming_damage < unit.hp,
        }
    }
}

/// 生産施設が1ターンに供給し得るunitの平均コスト（補充阻止価値）。
/// 敵の生産マスを1つ占位することは、敵の補充を毎ターン1体分止めることであり、
/// その価値は止めた補充の価値に等しいという教義に基づく。
/// 魔法定数を避け、マスターデータのunitカタログから導出する。
fn production_facility_blockade_value(terrain: Terrain, registry: &MasterDataRegistry) -> u32 {
    let producible_costs: Vec<u32> = registry
        .unit_order
        .iter()
        .filter_map(|name| {
            let record = registry.units.get(name)?;
            let unit_type = UnitType::from_str(&record.name.0)?;
            registry
                .can_produce_unit(terrain.as_str(), unit_type)
                .then_some(record.cost)
        })
        .collect();
    if producible_costs.is_empty() {
        0
    } else {
        producible_costs.iter().sum::<u32>() / producible_costs.len() as u32
    }
}

/// 敵の補充依存度を分数 (分子, 分母) で返す。占位の実効価値係数。
///
/// 占位が止められるのは「敵生産マス1つあたり毎ターン1体の補充」であり、
/// その価値は敵の次ターン戦力に占める補充の割合、すなわち
/// 生産能力 / (生産能力 + 現有戦力) に比例する。
///
/// - 敵が大軍を擁する序盤〜中盤はこの割合が小さい。補充1体を止める利得より、
///   自軍1体を占位に貼り付けて遊ばせる損のほうが大きいため、占位は事実上無効になる。
/// - 敵が消耗し、補充だけが生命線になった終盤で最大化し、全滅勝利を後押しする。
fn enemy_replacement_dependence(production_capacity: usize, fielded_units: usize) -> (u32, u32) {
    let capacity = production_capacity as u32;
    let denominator = capacity.saturating_add(fielded_units as u32);
    if denominator == 0 {
        // 生産能力も現有戦力もない敵に対して、止めるべき補充は存在しない。
        (0, 1)
    } else {
        (capacity, denominator)
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
}

impl SmallMapUnit {
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
            crate::ai::v4::capital_route_tactical_target(world, player_id, entity);
        // 回復中のDAG Entityは、所属をDAGに残したまま通常の回復探索を使う
        // （古いSquad目標を渡すと、修理ではなく横の拠点へ戻る）。
        let recovering = crate::ai::v4::capital_route_is_recovering(world, player_id, entity);
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
            advance_destination: crate::ai::v4::capital_route_advance_destination(
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
            is_solo,
            starts_on_own_production,
            candidate_tiles,
            has_non_production_wait_destination,
            can_end_turn_off_production,
            support_destination,
            repair_sites,
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
                && crate::ai::v4::capital_route_allows_capture_at(
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
                crate::ai::v4::capital_route_allows_tactical_position(
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
                    || !crate::ai::v4::is_capture_target_reserved(
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
        }
        // 間接部隊の支援射点: 前衛の目標を射程に収める位置へ加点し、射程外への前進を抑える。
        if board.is_v4
            && state.stats.min_range > 1
            && let Some(target) = unit.squad_target
        {
            let distance = tactical
                .map
                .distance(target.x, target.y, position.x, position.y);
            if (state.stats.min_range..=state.stats.max_range).contains(&distance) {
                score += 5000;
            }
        }
        if !unit.is_solo
            && let Some(target) = unit.squad_target
        {
            score += squad_target_approach_score(state, position, target, distances);
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

/// 1体のunitの全移動先・全行動のうち最良の (優先度, score) と命令。
fn best_action_for_unit(
    world: &World,
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    distances: &mut ApproachDistances,
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
        );
        consider_wait(board, unit, &tile, &mut best);
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
    if !tile.actions.can_capture
        || !crate::ai::v4::capital_route_allows_capture_at(
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
        let Some(priority) = attack_priority(
            board,
            unit,
            target,
            attack.target_position,
            exchange.is_favorable_matchup(),
            is_survivable_lethal_strike,
        ) else {
            continue;
        };
        let priority = if tile.relieves_gridlock {
            ActionPriority::ProductionSiteRelief
        } else if tile.blockade.holds_site {
            priority.max(ActionPriority::EnemyProductionBlockade)
        } else {
            priority
        };
        let rank = (priority, tile.base_score + attack.score);
        if tile.is_stationary && best_stationary_attack.is_none_or(|(best, _)| rank > best) {
            *best_stationary_attack = Some((rank, target));
        }
        best.offer(rank, || AiCommand::Attack {
            target_pos: tile.position,
            target_entity: target,
        });
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
    let is_strategic_target = if unit.is_capture_mission {
        blocks_capture_target
    } else {
        unit.deployment_target
            .map_or(unit.has_offensive_mission, |deployment| {
                deployment == target
            })
    };
    let has_strategic_mission =
        unit.deployment_target.is_some() || unit.has_offensive_mission || blocks_capture_target;
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

/// 待機。自軍生産口を塞ぐ待機と、campaign目標から遠ざかる待機はしない。
fn consider_wait(
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    tile: &TileCandidate,
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
    let mut score = tile.base_score + wait_recovery_score(state, repair_site);
    let holds_blockade = tile.blockade.holds_site;
    // 占位の価値は base_score で一度計上済みなので、居座りへ重ねて加点しない。
    // 敵生産口から始めた健全なunitが、占位を続けられない敵生産口で待機する場合は、
    // その占位価値の2倍を減点する（移動先が敵生産口でなければ価値は0）。
    if !holds_blockade && tile.blockade.starts_on_site && !state.combat_ineffective {
        score -= tile.blockade.site_value as i32 * 2;
    }
    let moves_away_from_campaign = unit.campaign.as_ref().is_some_and(|context| {
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
                tactical
                    .map
                    .distance(tile.position.x, tile.position.y, target.x, target.y)
                    > tactical
                        .map
                        .distance(state.position.x, state.position.y, target.x, target.y)
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
        tactical
            .enemies
            .iter()
            .any(|(enemy, _, _, hp, _, _, movement)| {
                *hp > 0
                    && tactical
                        .map
                        .distance(tile.position.x, tile.position.y, enemy.x, enemy.y)
                        <= *movement + extra_reach
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
    if !board.is_v4 || state.stats.min_range <= 1 || fire_in_place_blocks_production {
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

#[cfg(test)]
mod tests {
    use super::enemy_replacement_dependence;

    #[test]
    fn enemy_replacement_dependence_scales_with_reliance_on_production() {
        // 敵が大軍を擁する序盤〜中盤: 補充1体を止めても次ターン戦力への寄与は小さい。
        let (num, den) = enemy_replacement_dependence(4, 36);
        assert_eq!((num, den), (4, 40));
        assert!(
            num * 10 <= den,
            "大軍を持つ敵への占位価値は1割以下に割り引かれるべき"
        );

        // 敵が消耗し補充だけが生命線になった終盤: 占位価値が最大化する。
        let (num, den) = enemy_replacement_dependence(4, 2);
        assert_eq!((num, den), (4, 6));
        assert!(
            num * 2 > den,
            "消耗した敵への占位価値は半分より大きく評価されるべき"
        );

        // 敵が全滅していれば止めるべき補充はないが、生産能力は価値を保つ。
        assert_eq!(enemy_replacement_dependence(3, 0), (3, 3));

        // 敵の生産能力がなければ占位の価値はゼロ。
        assert_eq!(enemy_replacement_dependence(0, 10), (0, 10));

        // 生産能力も戦力もない敵に対しては、ゼロ除算せず価値ゼロを返す。
        assert_eq!(enemy_replacement_dependence(0, 0), (0, 1));
    }
}

pub(crate) mod squad_executor;
