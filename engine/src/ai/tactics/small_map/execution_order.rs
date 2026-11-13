//! 小規模V4の実行順。配置や採点は変えず、退去・支援射撃などの先行条件を解く。
//! 循環を許す依存グラフを有限走査し、1手を実行した後は実盤面から作り直す。
use super::{EvaluatedActions, RouteOrders, SmallMapBoard, SmallMapUnit};
use crate::ai::command::AiCommand;
use crate::ai::tactics::approach::ApproachDistances;
use crate::collections::{HashMap, HashSet};
use crate::components::GridPosition;
use crate::systems::movement::{
    OccupantInfo, calculate_reachable_tile_costs, calculate_reachable_tiles,
};
use bevy_ecs::prelude::*;

type Cell = (usize, usize);

/// 移動だけの部隊が、既存の目標へ向かう際の経路上の事実。
/// 修理・後退・作戦目標のない待機を、一般の前進として並べ替えない。
pub(super) struct MovementIntent {
    pub target: GridPosition,
    pub origin_mp: u32,
    pub destination_mp: u32,
    pub movement_type: crate::resources::MovementType,
    /// 現在の移動先より先にある、到達範囲内の友軍占有マス。
    pub blocked_stops: HashSet<GridPosition>,
    /// 自身の占有またはZOCが前進を妨げる敵。撃破したと仮定せず、実射撃後に再評価する。
    pub enemy_blockers: HashSet<Entity>,
    /// 同じ最短進路上にある、到達範囲内のマス。別の進路を全体距離で並べ替えない。
    pub forward_cells: HashSet<GridPosition>,
}

/// 先行条件検出専用の候補集合。DAGの到達先も与えられた到達集合から取り直す。
/// 命令として使う候補は、実盤面で既存の評価器が生成したものに限る。
fn scheduling_tiles(
    world: &World,
    board: &SmallMapBoard,
    unit: &SmallMapUnit,
    occupancy: &HashMap<Cell, OccupantInfo>,
) -> Vec<Cell> {
    let state = &unit.state;
    let reachable = calculate_reachable_tiles(
        &board.tactical.map,
        occupancy,
        (state.position.x, state.position.y),
        state.stats.movement_type,
        state.stats.max_movement,
        state.fuel,
        board.tactical.player_id,
        state.stats.unit_type,
        &board.tactical.registry,
    );
    let route = RouteOrders {
        tactical_target: unit.route.tactical_target,
        advance_destination:
            crate::ai::v4::capital_route::tactical::capital_route_advance_destination(
                world,
                board.tactical.player_id,
                state.entity,
                &reachable,
            ),
    };
    super::candidate_tiles(
        world,
        board,
        state,
        &route,
        unit.has_capture_target,
        unit.starts_on_own_production,
        unit.deployment_target,
        &reachable,
    )
}

impl MovementIntent {
    pub fn observe(
        world: &World,
        board: &SmallMapBoard,
        unit: &SmallMapUnit,
        command: &AiCommand,
        distances: &mut ApproachDistances,
    ) -> Option<Self> {
        let AiCommand::Wait { target_pos } = command else {
            return None;
        };
        let state = &unit.state;
        let waiting_for_escort = !unit.has_capture_target
            && unit.campaign.as_ref().is_some_and(|context| {
                context.mission_type == crate::ai::squad::MissionType::Capture
                    && !context.departure_authorized
            });
        if state.combat_ineffective || waiting_for_escort {
            return None;
        }
        // Some(None) はDAGのControl指令。古いSquad目標へ戻して前進させない。
        let target = unit.route.tactical_target.unwrap_or(unit.squad_target)?;
        let progress =
            distances.target_progress(&state.stats, state.position, *target_pos, target, 0);
        let (origin_mp, destination_mp) = (progress.current_path_mp?, progress.candidate_path_mp?);
        if destination_mp > origin_mp {
            return None;
        }
        let mut result = Self {
            target,
            origin_mp,
            destination_mp,
            movement_type: state.stats.movement_type,
            blocked_stops: HashSet::default(),
            forward_cells: HashSet::default(),
            enemy_blockers: HashSet::default(),
        };
        let enemies = board
            .tactical
            .occupancy
            .iter()
            .filter(|(_, occupant)| occupant.player_id != board.tactical.player_id)
            .map(|(cell, occupant)| (*cell, occupant.clone()))
            .collect::<HashMap<_, _>>();
        // 逆向きの全盤面距離表を候補マスごとに作らず、起点から届く範囲のコストを一度だけ求める。
        // 敵占有/ZOCを外した結果は検出用であり、これだけを理由に移動を実行しない。
        let open = calculate_reachable_tile_costs(
            &board.tactical.map,
            &HashMap::default(),
            (state.position.x, state.position.y),
            state.stats.movement_type,
            state.stats.max_movement,
            state.fuel,
            board.tactical.player_id,
            state.stats.unit_type,
            &board.tactical.registry,
        );
        for (x, y) in scheduling_tiles(world, board, unit, &enemies) {
            let cell = GridPosition { x, y };
            let Some(remaining) = distances
                .target_progress(&state.stats, state.position, cell, target, 0)
                .candidate_path_mp
            else {
                continue;
            };
            if remaining >= origin_mp {
                continue;
            }
            if remaining < destination_mp
                && board
                    .tactical
                    .occupancy
                    .get(&(x, y))
                    .is_some_and(|other| other.player_id == board.tactical.player_id)
            {
                result.blocked_stops.insert(cell);
            }
            if open
                .get(&(x, y))
                .is_some_and(|cost| cost.saturating_add(remaining) == origin_mp)
            {
                result.forward_cells.insert(cell);
            }
        }
        result.observe_enemy_blockers(world, board, unit, distances, &enemies, &open);
        Some(result)
    }

    /// 敵のマスだけでなく、そのZOCが届く範囲を塞ぐ場合も、除去後に改善する候補があるか調べる。
    /// 一体ずつ除去して他の敵・地形・燃料・DAG制約は維持する。射撃の結果は適用後に確認する。
    fn observe_enemy_blockers(
        &mut self,
        world: &World,
        board: &SmallMapBoard,
        unit: &SmallMapUnit,
        distances: &mut ApproachDistances,
        enemies: &HashMap<Cell, OccupantInfo>,
        open: &std::collections::BTreeMap<Cell, u32>,
    ) {
        let state = &unit.state;
        let before = scheduling_tiles(world, board, unit, enemies)
            .into_iter()
            .collect::<HashSet<_>>();
        for enemy in board
            .tactical
            .action_targets
            .units()
            .iter()
            .filter(|enemy| {
                enemy.faction != board.tactical.player_id
                    && !enemy.is_transporting
                    && (open.contains_key(&(enemy.position.x, enemy.position.y))
                        || board
                            .tactical
                            .map
                            .get_adjacent(enemy.position.x, enemy.position.y)
                            .iter()
                            .any(|cell| open.contains_key(cell)))
            })
        {
            let mut without_enemy = enemies.clone();
            without_enemy.remove(&(enemy.position.x, enemy.position.y));
            let improves = scheduling_tiles(world, board, unit, &without_enemy)
                .into_iter()
                .any(|(x, y)| {
                    // 射撃前から選べるマスは、射撃の必要性を示さない。
                    if before.contains(&(x, y)) {
                        return false;
                    }
                    // 自身が占有していたセルが空く場合だけは、その敵の現在の占有を無視する。
                    if board.tactical.occupancy.contains_key(&(x, y))
                        && (GridPosition { x, y }) != enemy.position
                    {
                        return false;
                    }
                    distances
                        .target_progress(
                            &state.stats,
                            state.position,
                            GridPosition { x, y },
                            self.target,
                            0,
                        )
                        .candidate_path_mp
                        .is_some_and(|remaining| remaining < self.destination_mp)
                });
            if improves {
                self.enemy_blockers.insert(enemy.entity);
            }
        }
    }
}

/// actionの添字 → 先に実行すべきactionの添字。循環もありうる。
struct Dependencies {
    before: Vec<Vec<usize>>,
}

impl Dependencies {
    /// 同じ盤面で各ノードを高々1回展開する。閉塞なら既存の先頭手へ戻り、保留だけを返さない。
    fn resolve(&self, order: &[usize]) -> Option<(usize, usize)> {
        let mut visited = vec![false; self.before.len()];
        for &root in order {
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                if visited[node] {
                    continue;
                }
                visited[node] = true;
                if self.before[node].is_empty() {
                    return Some((root, node));
                }
                stack.extend(self.before[node].iter().rev().copied());
            }
        }
        order.first().map(|&root| (root, root))
    }
}

/// 既存の候補を並べ替えるだけ。未来の移動先を命令へ変換しない。
pub(super) fn select(
    world: &World,
    batch: &EvaluatedActions,
    order: &[usize],
    support: &[(usize, usize)],
) -> Option<(usize, usize)> {
    let mut graph = Dependencies {
        before: vec![Vec::new(); batch.actions.len()],
    };
    for &(front, gun) in support {
        graph.before[front].push(gun);
    }
    for &rear_index in order {
        let rear = &batch.actions[rear_index];
        let Some(rear_move) = &rear.movement else {
            continue;
        };
        for &front_index in order {
            if front_index == rear_index {
                continue;
            }
            let front = &batch.actions[front_index];
            let same_operation = super::squad_executor::shares_attack_operation(
                world,
                rear.entity,
                front.entity,
                batch.board.squads.target(front.entity) == Some(rear_move.target),
            );
            if let AiCommand::Attack { target_entity, .. } = front.command
                && rear_move.enemy_blockers.contains(&target_entity)
                && same_operation
                && batch.board.squads.mission(front.entity)
                    == Some(&crate::ai::squad::MissionType::Attack)
            {
                graph.before[rear_index].push(front_index);
            }
            // 命令を書き換えず、実際に退去する移動攻撃・占領・合流も先行条件として利用する。
            let destination = match front.command {
                AiCommand::Wait { target_pos }
                | AiCommand::Attack { target_pos, .. }
                | AiCommand::Capture { target_pos }
                | AiCommand::Merge { target_pos, .. }
                | AiCommand::Load { target_pos, .. }
                | AiCommand::Supply { target_pos, .. } => target_pos,
                AiCommand::Drop {
                    transport_target_pos,
                    ..
                } => transport_target_pos,
            };
            if destination == front.origin {
                continue;
            }
            let clears_stop = rear_move.blocked_stops.contains(&front.origin);
            // 独立移動の前後は同作戦・同目標・同移動種別・同任務の最短進路内だけで比較する。
            let front_first = same_operation
                && front.movement.as_ref().is_some_and(|front_move| {
                    rear_move.target == front_move.target
                        && rear_move.forward_cells.contains(&front.origin)
                        && front_move.destination_mp < front_move.origin_mp
                        && batch.board.squads.mission(rear.entity)
                            == batch.board.squads.mission(front.entity)
                        && rear_move.movement_type == front_move.movement_type
                });
            if clears_stop || front_first {
                graph.before[rear_index].push(front_index);
            }
        }
    }
    graph.resolve(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 敵を除去しても到達集合が変わらなければ、射撃を移動の先行条件にしない。
    #[test]
    fn enemy_removal_must_open_a_new_stop_not_just_find_an_existing_advance() {
        use crate::components::{
            ActionCompleted, Faction, Fuel, HasMoved, Health, PlayerId, UnitStats,
        };
        use crate::resources::master_data::{MasterDataRegistry, UnitName};
        use crate::resources::{GridTopology, Map, Terrain, UnitType};
        let mut world = World::new();
        world.insert_resource(Map::new(7, 2, Terrain::Plains, GridTopology::Square));
        world.insert_resource(crate::resources::DamageChart::new());
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(PlayerId(1), crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(settings);
        let mut stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&UnitName(UnitType::SupplyTruck.as_str().to_owned()))
            .unwrap();
        stats.max_movement = 3;
        let source = world
            .spawn((
                stats,
                GridPosition { x: 0, y: 0 },
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: 99,
                    max: 99,
                },
                HasMoved(false),
                ActionCompleted(false),
            ))
            .id();
        let enemy_stats: UnitStats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&UnitName(UnitType::Tank.as_str().to_owned()))
            .unwrap();
        world.spawn((
            enemy_stats,
            GridPosition { x: 3, y: 1 },
            Faction(PlayerId(2)),
            Health {
                current: 100,
                max: 100,
            },
            HasMoved(false),
            ActionCompleted(false),
        ));
        let mut manager = crate::ai::squad::SquadManager::new();
        let mut squad =
            manager.create_owned_squad(crate::ai::squad::MissionType::Attack, PlayerId(1));
        squad.members.insert(source);
        squad.plan.target = Some(GridPosition { x: 6, y: 0 });
        squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager.clone());
        let (_, occupancy) = crate::ai::tactics::board::movable_units_and_occupancy(
            &mut world,
            PlayerId(1),
            &HashSet::default(),
            None,
        );
        let squads =
            crate::ai::tactics::board::SquadRoles::for_small_map(&world, PlayerId(1), manager);
        let tactical = crate::ai::tactics::board::TacticalBoard::observe(
            &mut world,
            PlayerId(1),
            occupancy,
            squads,
        );
        let board = SmallMapBoard::observe(&world, tactical);
        crate::ai::tactics::approach::with_turn_distances(
            &mut world,
            &board.tactical,
            |world, distances| {
                let unit = SmallMapUnit::observe(world, &board, source, distances).unwrap();
                // 合法な同位置待機を仮に選んでも、既に移動できることと射撃の必要性を混同しない。
                let movement = MovementIntent::observe(
                    world,
                    &board,
                    &unit,
                    &AiCommand::Wait {
                        target_pos: GridPosition { x: 0, y: 0 },
                    },
                    distances,
                )
                .unwrap();
                assert!(movement.enemy_blockers.is_empty());
            },
        );
    }

    /// 依存が連鎖しても、根の待機ではなく実行できる末端へ進む。
    #[test]
    fn prerequisite_chain_selects_the_ready_action() {
        let graph = Dependencies {
            before: vec![vec![1], vec![2], vec![]],
        };
        assert_eq!(graph.resolve(&[0, 1, 2]), Some((0, 2)));
    }

    /// 循環だけなら一巡で既存の合法手へ戻る。
    #[test]
    fn cycle_falls_back_without_retrying_the_same_board() {
        let graph = Dependencies {
            before: vec![vec![1], vec![0]],
        };
        assert_eq!(graph.resolve(&[0, 1]), Some((0, 0)));
    }

    /// 一部が循環していても、独立した実行可能な行動を失わない。
    #[test]
    fn independent_action_runs_before_a_deadlocked_component() {
        let graph = Dependencies {
            before: vec![vec![1], vec![0], vec![]],
        };
        assert_eq!(graph.resolve(&[0, 1, 2]), Some((2, 2)));
    }
}
