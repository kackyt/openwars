//! 小規模V4のSquad単位の行動実行順と前衛・間接支援の連携。
use crate::ai::command::AiCommand;
use crate::ai::context::tactical::*;
use crate::collections::{HashMap, HashSet};
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, Health, PlayerId, Property, UnitStats,
};
use crate::resources::Map;
use crate::resources::master_data::MasterDataRegistry;
use crate::systems::combat::get_expected_damage;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles, get_valid_movement_cost};
use bevy_ecs::prelude::*;

/// V4 Squad executorが同じ任務だけを連続実行しないための巡回位置。
///
/// Entity単位の全体順位へ戻さず、Squadを命令主体に保ったまま、攻勢・占領・防衛の
/// 進行を1アクションずつ交互にする。
#[derive(Resource, Debug, Default)]
struct V4SquadExecutionCursor {
    last_squad: HashMap<PlayerId, crate::ai::squad::SquadId>,
}

/// 実行順ソート済みの1つのSquadエントリ: (任務優先度, SquadId, 所属Entity集合, 目標座標)。
type ExecutableSquadEntry = (u8, u32, HashSet<Entity>, Option<GridPosition>);

/// Squad executorから渡されたmember集合だけを戦術採点する内部入口。
///
/// V2/V3の公開関数は従来どおり全unitを対象にする。V4だけはSquadが選んだ
/// candidate集合を渡すため、ここは戦略目標を新たに発明せず、Move/Attack/Captureの
/// 戦術順位だけを決める。
fn decide_ai_action_v2_for_entities(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &crate::collections::HashSet<Entity>,
    permitted_entities: Option<&HashSet<Entity>>,
) -> Option<(Entity, AiCommand)> {
    crate::ai::tactics::small_map::decide_action(
        world,
        player_id,
        skip_entities,
        permitted_entities,
    )
}

/// 台帳の作戦所有権を優先し、未登録の通常Squadだけ目標一致で協働する。
fn shares_attack_operation(world: &World, left: Entity, right: Entity, same_target: bool) -> bool {
    world
        .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
        .and_then(|registry| registry.shares_attack_operation(left, right))
        .unwrap_or(same_target)
}

/// 前衛の現在位置に追随する後方射点を選ぶ。前衛の将来位置を既成事実にしない。
/// 射撃可能なら移動しない。敵の合法な移動後攻撃が届くセルも追随先にしない。
#[allow(clippy::too_many_arguments)]
pub(crate) fn coordinated_support_destination(
    world: &World,
    player: PlayerId,
    gun: Entity,
    gun_stats: &UnitStats,
    origin: GridPosition,
    objective: Option<GridPosition>,
    candidates: &[(usize, usize)],
    occupants: &HashMap<(usize, usize), OccupantInfo>,
    manager: &crate::ai::squad::SquadManager,
    snapshot: &AiTacticalSnapshot,
    map: &Map,
    master: &MasterDataRegistry,
    chart: &crate::resources::DamageChart,
) -> Option<GridPosition> {
    let objective = objective?;
    if !snapshot
        .action_targets_at(gun, gun_stats, origin, false)
        .attackable_targets
        .is_empty()
    {
        return None;
    }
    let attack_squads = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.plan.owner_id == Some(player)
                && squad.plan.mission_type == crate::ai::squad::MissionType::Attack
        })
        .collect::<Vec<_>>();
    if !attack_squads
        .iter()
        .any(|squad| squad.members.contains(&gun))
    {
        return None;
    }
    let front_distance = attack_squads
        .iter()
        .flat_map(|squad| {
            squad.members.iter().filter(move |entity| {
                shares_attack_operation(world, gun, **entity, squad.plan.target == Some(objective))
            })
        })
        .filter_map(|entity| {
            let stats = world.get::<UnitStats>(*entity)?;
            let hp = world.get::<Health>(*entity)?;
            let pos = world.get::<GridPosition>(*entity)?;
            // 占領担当を引き抜かず、この攻勢に所属する生存直接部隊だけを見る。
            (stats.min_range <= 1
                && hp.current > 0
                && world
                    .get::<crate::components::Transporting>(*entity)
                    .is_none())
            .then_some(map.distance(pos.x, pos.y, objective.x, objective.y))
        })
        .min()?;
    let target = snapshot
        .units()
        .iter()
        .find(|unit| {
            unit.position == objective
                && unit.faction != player
                && !unit.is_transporting
                && world
                    .get::<Health>(unit.entity)
                    .is_some_and(|hp| hp.current > 0)
        })?
        .entity;
    let mut destinations = candidates
        .iter()
        .filter_map(|&(x, y)| {
            let pos = GridPosition { x, y };
            let distance = map.distance(x, y, objective.x, objective.y);
            (pos != origin
                && !occupants.contains_key(&(x, y))
                && distance > front_distance
                && distance < map.distance(origin.x, origin.y, objective.x, objective.y)
                && snapshot
                    .action_targets_at(gun, gun_stats, pos, false)
                    .attackable_targets
                    .contains(&target)
                && !world.iter_entities().any(|entity| {
                    entity.get::<GridPosition>().copied() == Some(pos)
                        && entity.get::<Property>().is_some_and(|property| {
                            master.is_production_facility(property.terrain.as_str())
                        })
                }))
            .then_some(pos)
        })
        .collect::<Vec<_>>();
    // 射程を満たす最後方のセルから検査する。同距離は移動量・座標で安定化する。
    destinations.sort_unstable_by_key(|pos| {
        (
            std::cmp::Reverse(map.distance(pos.x, pos.y, objective.x, objective.y)),
            map.distance(origin.x, origin.y, pos.x, pos.y),
            pos.y,
            pos.x,
        )
    });
    if destinations.is_empty() {
        return None;
    }
    // 砲の元の位置は空く。味方前衛・敵を含む他の占有はそのまま経路計算へ渡す。
    let mut future_occupants = occupants.clone();
    future_occupants.remove(&(origin.x, origin.y));
    let enemies = snapshot
        .units()
        .iter()
        .filter_map(|unit| {
            if unit.faction == player || unit.is_transporting {
                return None;
            }
            let stats = world.get::<UnitStats>(unit.entity)?;
            let hp = world.get::<Health>(unit.entity)?.current;
            if hp == 0 {
                return None;
            }
            let fuel = world
                .get::<crate::components::Fuel>(unit.entity)
                .map_or(0, |f| f.current);
            let reachable = calculate_reachable_tiles(
                map,
                &future_occupants,
                (unit.position.x, unit.position.y),
                stats.movement_type,
                stats.max_movement,
                fuel,
                unit.faction,
                stats.unit_type,
                master,
            );
            Some((unit, stats, hp, reachable))
        })
        .collect::<Vec<_>>();
    destinations.into_iter().find(|pos| {
        let defense = map
            .get_terrain(pos.x, pos.y)
            .map_or(0, |terrain| master.get_terrain_defense_bonus(terrain));
        !enemies.iter().any(|(unit, stats, hp, reachable)| {
            reachable
                .iter()
                .chain(std::iter::once(&(unit.position.x, unit.position.y)))
                .any(|&(x, y)| {
                    if future_occupants.contains_key(&(x, y))
                        && (x, y) != (unit.position.x, unit.position.y)
                    {
                        return false;
                    }
                    let distance = map.distance(x, y, pos.x, pos.y);
                    // 実ゲームと同じく、移動後には遠距離射撃できない。
                    if (x, y) != (unit.position.x, unit.position.y) && distance > 1 {
                        return false;
                    }
                    get_expected_damage(
                        stats,
                        *hp,
                        (unit.ammo1, unit.ammo2),
                        gun_stats,
                        defense,
                        distance,
                        master,
                        chart,
                        false,
                    ) > 0
                })
        })
    })
}

/// V4の通常行動を、Squadを命令主体として選ぶ。
///
/// 先にRoadmap/Reconcilerが確定した唯一所属Squadを優先順に取り出し、そのmemberだけを
/// 既存の戦術採点器へ渡す。従ってこの層は別の島・別NodeへEntityを再配分せず、
/// `AiCommand` がEntity指定であるというエンジン境界だけを最後に解決する。
pub(crate) fn decide_squad_action_v4(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let squads = collect_executable_squads(world, player_id);
    let start = world
        .get_resource::<V4SquadExecutionCursor>()
        .and_then(|cursor| cursor.last_squad.get(&player_id).copied())
        .and_then(|last_squad| {
            squads
                .iter()
                .position(|(_, squad_id, _, _)| *squad_id == last_squad.0)
                .map(|index| (index + 1) % squads.len())
        })
        .unwrap_or(0);
    let mut deferred_wait = None;
    for offset in 0..squads.len() {
        let index = (start + offset) % squads.len();
        let (priority, squad_id, members, squad_target) = &squads[index];
        let Some(action) =
            decide_ai_action_v2_for_entities(world, player_id, skip_entities, Some(members))
        else {
            continue;
        };
        if let Some(support_action) = try_stationary_support_fire_before_frontline(
            world,
            player_id,
            *priority,
            *squad_target,
            &action,
            &squads,
            skip_entities,
        ) {
            // 巡回位置を進めず、砲撃後は前衛から再評価する。
            return Some(support_action);
        }
        // 目的方向の出口を、まだ行動できる友軍が塞いでいる場合だけ同位置Waitを
        // 一巡保留する。先に出口側Squadを動かした次のAI stepで同じEntityを再評価し、
        // 生産拠点内の後続を行動済みにしてから出口が空く順序逆転を防ぐ。
        if wait_is_temporarily_blocked_by_unacted_friendly(
            world,
            player_id,
            action.0,
            &action.1,
            *squad_target,
            skip_entities,
        ) {
            deferred_wait.get_or_insert((*squad_id, action));
            continue;
        }
        // 保留済みSquadがある間は巡回位置を進めない。退去行動を実行した次stepを
        // 同じ走査位置から始め、空いた出口を他のSquadに再占有される前に再評価する。
        if deferred_wait.is_none() {
            update_v4_squad_execution_cursor(world, player_id, *squad_id);
        }
        return Some(action);
    }
    // 全Squadを一巡して退去・攻撃などを選べなければ、最初のWaitを確定する。
    // 保留だけを返さず再走査する構造にしないことで、相互閉塞時も無限deferしない。
    deferred_wait.map(|(squad_id, action)| {
        update_v4_squad_execution_cursor(world, player_id, squad_id);
        action
    })
}

/// 自軍の非Transport Squadを、任務優先度とSquadIdで安定ソートして返す。
fn collect_executable_squads(world: &World, player_id: PlayerId) -> Vec<ExecutableSquadEntry> {
    let manager = world
        .get_resource::<crate::ai::squad::SquadManager>()
        .cloned()
        .unwrap_or_default();
    let assignments = world
        .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .map(|registry| registry.player_assignments(player_id));
    let mut squads = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.plan.owner_id == Some(player_id)
                && squad.plan.mission_type != crate::ai::squad::MissionType::Transport
        })
        .filter_map(|squad| {
            let members = squad
                .members
                .iter()
                .copied()
                .filter(|entity| {
                    assignments.as_ref().is_none_or(|assignments| {
                        assignments
                            .get(entity)
                            .is_none_or(|assignment| assignment.squad_id == Some(squad.id))
                    })
                })
                .collect::<HashSet<_>>();
            (!members.is_empty()).then_some((
                squad_executor_priority(&squad.plan.mission_type),
                squad.id.0,
                members,
                squad.plan.target,
            ))
        })
        .collect::<Vec<_>>();
    // 同一任務のSquad間はIDで安定化する。乱数を持たないため、評価試合のseedを跨いで
    // 意思決定順が揺れない。
    squads.sort_unstable_by_key(|(priority, squad_id, _, _)| (*priority, *squad_id));
    squads
}

/// 同じ攻略目標の前衛を行動済みにする前に、後方からの合法な静止砲撃があれば先に返す。
/// 火力を仮想的に被害から差し引かず、次のAI stepで実際の敵HPを読み直す。
/// Capture/Defenseの担当は引き抜かず、攻勢Squad同士に限定する。
fn try_stationary_support_fire_before_frontline(
    world: &mut World,
    player_id: PlayerId,
    squad_priority: u8,
    squad_target: Option<GridPosition>,
    front_action: &(Entity, AiCommand),
    squads: &[ExecutableSquadEntry],
    skip_entities: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let attack_priority = squad_executor_priority(&crate::ai::squad::MissionType::Attack);
    if squad_priority != attack_priority
        || !world
            .get::<UnitStats>(front_action.0)
            .is_some_and(|stats| stats.min_range <= 1)
        || !matches!(
            front_action.1,
            AiCommand::Attack { .. } | AiCommand::Wait { .. }
        )
    {
        return None;
    }
    let objective = squad_target?;
    let support_target = match &front_action.1 {
        AiCommand::Attack { target_entity, .. } => Some(*target_entity),
        // 足止めされている前衛も、目的地の敵が削られる前にWaitを確定しない。
        _ => world.iter_entities().find_map(|entity| {
            (entity.get::<GridPosition>().copied() == Some(objective)
                && entity.get::<Faction>().is_some_and(|f| f.0 != player_id)
                && entity.get::<Health>().is_some_and(|h| h.current > 0)
                && entity.get::<UnitStats>().is_some())
            .then_some(entity.id())
        }),
    }?;

    for (priority, _, support_members, support_objective) in squads {
        if *priority != squad_priority {
            continue;
        }
        let guns = support_members
            .iter()
            .copied()
            .filter(|entity| {
                world
                    .get::<UnitStats>(*entity)
                    .is_some_and(|stats| stats.min_range > 1)
                    && world.get::<HasMoved>(*entity).is_some_and(|moved| !moved.0)
                    && shares_attack_operation(
                        world,
                        front_action.0,
                        *entity,
                        *support_objective == Some(objective),
                    )
            })
            .collect::<HashSet<_>>();
        if guns.is_empty() {
            continue;
        }
        if let Some((gun, command)) =
            decide_ai_action_v2_for_entities(world, player_id, skip_entities, Some(&guns))
        {
            // 通常評価でも選ばれた、その場からの同一目標への射撃だけを実行。
            // 移動先での架空射撃や、別任務の砲の徴用はしない。
            if let AiCommand::Attack { target_pos, .. } = &command
                && world.get::<GridPosition>(gun) == Some(target_pos)
                && crate::systems::action::get_tactical_action_targets_at(
                    world,
                    gun,
                    *target_pos,
                    false,
                )
                .attackable_targets
                .contains(&support_target)
            {
                return Some((
                    gun,
                    AiCommand::Attack {
                        target_pos: *target_pos,
                        target_entity: support_target,
                    },
                ));
            }
        }
    }
    None
}

fn update_v4_squad_execution_cursor(world: &mut World, player_id: PlayerId, squad_id: u32) {
    let mut cursor = world
        .remove_resource::<V4SquadExecutionCursor>()
        .unwrap_or_default();
    cursor
        .last_squad
        .insert(player_id, crate::ai::squad::SquadId(squad_id));
    world.insert_resource(cursor);
}

/// 同位置Waitが、目的地へ近づく全ての隣接出口を未行動友軍に塞がれた一時停止か判定する。
///
/// 敵・行動済み友軍・地形による閉塞は同じ手番中に解消する保証がないため保留しない。
/// DAGが一点目標を外したControl中も、古いSquad目標を復活させず通常のWaitを確定する。
fn wait_is_temporarily_blocked_by_unacted_friendly(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    command: &AiCommand,
    squad_target: Option<GridPosition>,
    skip_entities: &HashSet<Entity>,
) -> bool {
    let AiCommand::Wait { target_pos } = command else {
        return false;
    };
    let Some(position) = world.get::<GridPosition>(entity).copied() else {
        return false;
    };
    if *target_pos != position {
        return false;
    }
    let objective = match crate::ai::v4::capital_route_tactical_target(world, player_id, entity) {
        Some(target) => target,
        None => squad_target,
    };
    let Some(objective) = objective.filter(|target| *target != position) else {
        return false;
    };
    let Some(map) = world.get_resource::<Map>() else {
        return false;
    };
    let Some(master_data) = world.get_resource::<MasterDataRegistry>() else {
        return false;
    };
    let Some(stats) = world.get::<UnitStats>(entity) else {
        return false;
    };
    let current_distance = map.distance(position.x, position.y, objective.x, objective.y);
    let mut found_toward_exit = false;

    for (x, y) in map.get_adjacent(position.x, position.y) {
        if map.distance(x, y, objective.x, objective.y) >= current_distance {
            continue;
        }
        let Some(terrain) = map.get_terrain(x, y) else {
            continue;
        };
        if get_valid_movement_cost(master_data, stats.movement_type, terrain).is_none() {
            continue;
        }
        found_toward_exit = true;
        let blocker = world.iter_entities().find_map(|candidate| {
            if candidate.id() == entity
                || candidate.get::<crate::components::Transporting>().is_some()
                || candidate.get::<GridPosition>().copied() != Some(GridPosition { x, y })
            {
                return None;
            }
            candidate
                .get::<Faction>()
                .map(|faction| (candidate.id(), faction.0))
        });
        let Some((blocker, faction)) = blocker else {
            return false;
        };
        let blocker_unacted = faction == player_id
            && !skip_entities.contains(&blocker)
            && world.get::<HasMoved>(blocker).is_some_and(|moved| !moved.0)
            && world
                .get::<ActionCompleted>(blocker)
                .is_some_and(|completed| !completed.0);
        if !blocker_unacted {
            return false;
        }
    }

    found_toward_exit
}

/// 敵前のCapture Squadを単独で先走らせないよう、まずSuppress/Attackを扱い、残りは
/// Capture・Defense・Reserveの順にする。
///
/// これは戦術の点数を置換するものではなく、複数Squadのうちどの命令を次に展開するか
/// を決めるRoadmap投影順である。
fn squad_executor_priority(mission: &crate::ai::squad::MissionType) -> u8 {
    match mission {
        crate::ai::squad::MissionType::Attack => 0,
        crate::ai::squad::MissionType::Capture => 1,
        crate::ai::squad::MissionType::Defense => 2,
        crate::ai::squad::MissionType::Reserve => 3,
        crate::ai::squad::MissionType::Transport => u8::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::execute_ai_turn;
    use crate::ai::runtime::state::AiActionCooldown;
    use crate::ai::runtime::turn::execute_turn_step as execute_ai_turn_v2;
    use crate::ai::tactics::v2::decide_action as decide_ai_action_v2;
    use crate::components::{Faction, Health, PlayerId, Property, UnitStats};
    use crate::events::AttackUnitCommand;
    use crate::resources::master_data::UnitName;
    use crate::resources::{DamageChart, UnitType};
    use crate::resources::{GridTopology, Terrain};

    /// 弾薬を備えた前衛と砲を同一攻略目標へ割り当てる。
    fn setup_cooperative_fire_world() -> (World, Entity, Entity, Entity) {
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(6, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map::new(6, 3, Terrain::Plains, GridTopology::Square));
        world.insert_resource(DamageChart::new());
        let stats = |world: &World, kind: UnitType| {
            world
                .resource::<MasterDataRegistry>()
                .create_unit_stats(&UnitName(kind.as_str().to_owned()))
                .unwrap()
        };
        let front_stats = stats(&world, UnitType::Recon);
        let gun_stats = stats(&world, UnitType::LightSpGun);
        let enemy_stats = stats(&world, UnitType::Mech);
        let front = spawn_v3_test_unit(&mut world, player, 2, 100, front_stats);
        let gun = spawn_v3_test_unit(&mut world, player, 0, 100, gun_stats);
        let enemy = spawn_v3_test_unit(&mut world, PlayerId(2), 3, 100, enemy_stats);
        // 共通spawn helperは弾薬を付けないため、実際に射撃できる盤面を明示する。
        for entity in [front, gun, enemy] {
            let stats = world.get::<UnitStats>(entity).unwrap();
            let ammo = crate::components::Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            };
            world.entity_mut(entity).insert(ammo);
        }
        let target = GridPosition { x: 3, y: 0 };
        let mut manager = crate::ai::squad::SquadManager::new();
        for entity in [front, gun] {
            let mut squad =
                manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
            squad.members.insert(entity);
            squad.plan.target = Some(target);
            squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        }
        world.insert_resource(manager);
        (world, front, gun, enemy)
    }

    /// 同じ敵を攻略する前衛と砲がいる場合、編成順で前衛を先に消耗させない。
    #[test]
    fn v4_frontline_waits_for_stationary_support_fire() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let (mut world, front, gun, enemy) = setup_cooperative_fire_world();
        let (actor, command) =
            decide_squad_action_v4(&mut world, player, &HashSet::default()).unwrap();
        assert_eq!(
            actor, gun,
            "前衛より後方の静止射撃を先に実行する: {command:?}"
        );
        assert!(
            matches!(command, AiCommand::Attack { target_entity, target_pos }
            if target_entity == enemy && target_pos == GridPosition { x: 0, y: 0 })
        );

        // 仮想ダメージでなく、本番の戦闘SystemでHP・弾薬・行動済みを更新する。
        world.insert_resource(crate::resources::MatchState {
            current_phase: crate::resources::Phase::Main,
            ..Default::default()
        });
        world.insert_resource(crate::resources::Players(vec![
            crate::resources::Player::new(1, "P1".to_owned()),
            crate::resources::Player::new(2, "P2".to_owned()),
        ]));
        world.insert_resource(crate::resources::GameRng::new(42));
        world.insert_resource(Events::<AttackUnitCommand>::default());
        world.insert_resource(Events::<crate::events::UnitAttackedEvent>::default());
        world.insert_resource(Events::<crate::events::UnitDestroyedEvent>::default());
        world.send_event(AttackUnitCommand {
            attacker_entity: gun,
            defender_entity: enemy,
        });
        let mut schedule = Schedule::default();
        schedule.add_systems(crate::systems::combat::attack_unit_system);
        schedule.run(&mut world);
        assert!(world.get::<Health>(enemy).unwrap().current < 100);
        assert!(!world.get::<ActionCompleted>(front).unwrap().0);
        let (actor, command) =
            decide_squad_action_v4(&mut world, player, &HashSet::default()).unwrap();
        assert_eq!(actor, front, "砲撃後に前衛を再評価する: {command:?}");
        assert!(
            matches!(command, AiCommand::Attack { target_entity, .. } if target_entity == enemy)
        );
    }

    #[test]
    fn v4_same_operation_supports_front_with_a_different_squad_target() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let (mut world, front, gun, enemy) = setup_cooperative_fire_world();
        world
            .resource_mut::<crate::ai::squad::SquadManager>()
            .squads
            .iter_mut()
            .find(|s| s.members.contains(&gun))
            .unwrap()
            .plan
            .target = Some(GridPosition { x: 5, y: 2 });
        let mut deployments = crate::ai::v4::deployment::V4DeploymentRegistry::default();
        deployments.assign_target_for_test(PlayerId(1), front, enemy);
        deployments.assign_target_for_test(PlayerId(1), gun, enemy);
        world.insert_resource(deployments);
        let (actor, command) =
            decide_squad_action_v4(&mut world, PlayerId(1), &HashSet::default()).unwrap();
        assert_eq!(
            actor, gun,
            "Squadの一点目標が異なっても作戦所属を使う: {command:?}"
        );
        assert!(
            matches!(command, AiCommand::Attack { target_entity, .. } if target_entity == enemy)
        );
    }

    #[test]
    fn v4_unavailable_support_does_not_hold_the_frontline() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        for mode in 0..5 {
            let (mut world, front, gun, _) = setup_cooperative_fire_world();
            match mode {
                0 => world.get_mut::<HasMoved>(gun).unwrap().0 = true,
                1 => world.get_mut::<ActionCompleted>(gun).unwrap().0 = true,
                2 => {
                    let mut ammo = world.get_mut::<crate::components::Ammo>(gun).unwrap();
                    ammo.ammo1 = 0;
                    ammo.ammo2 = 0;
                }
                3 => {
                    // 別の作戦へ向かう砲の任務を横取りしない。
                    let mut manager = world.resource_mut::<crate::ai::squad::SquadManager>();
                    manager
                        .squads
                        .iter_mut()
                        .find(|s| s.members.contains(&gun))
                        .unwrap()
                        .plan
                        .target = Some(GridPosition { x: 5, y: 2 });
                }
                _ => {
                    world.despawn(gun);
                }
            }
            let (actor, _) =
                decide_squad_action_v4(&mut world, PlayerId(1), &HashSet::default()).unwrap();
            assert_eq!(
                actor, front,
                "支援が実行できなくても前衛を停止させない: mode={mode}"
            );
        }
    }

    /// 前衛が進んだ後、砲は遮断された敵経路の手前の射点へ追随する。
    #[test]
    fn v4_support_follows_advanced_front_without_passing_it() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(9, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(DamageChart::new());
        let master = world.resource::<MasterDataRegistry>().clone();
        let gun_stats = master
            .create_unit_stats(&UnitName(UnitType::LightSpGun.as_str().to_owned()))
            .unwrap();
        let front_stats = master
            .create_unit_stats(&UnitName(UnitType::MdTank.as_str().to_owned()))
            .unwrap();
        let enemy_stats = master
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let gun = spawn_v3_test_unit(&mut world, player, 1, 100, gun_stats);
        let front = spawn_v3_test_unit(&mut world, player, 5, 100, front_stats);
        let enemy = spawn_v3_test_unit(&mut world, PlayerId(2), 7, 100, enemy_stats);
        for entity in [front, gun, enemy] {
            let stats = world.get::<UnitStats>(entity).unwrap();
            let ammo = crate::components::Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            };
            world.entity_mut(entity).insert(ammo);
        }
        world.get_mut::<HasMoved>(front).unwrap().0 = true;
        world.get_mut::<ActionCompleted>(front).unwrap().0 = true;
        let mut manager = crate::ai::squad::SquadManager::new();
        for entity in [front, gun] {
            let mut squad =
                manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
            squad.members.insert(entity);
            squad.plan.target = Some(GridPosition { x: 7, y: 0 });
            squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        }
        world.insert_resource(manager);
        let (actor, command) =
            decide_squad_action_v4(&mut world, player, &HashSet::default()).unwrap();
        assert_eq!(actor, gun);
        assert!(
            matches!(command, AiCommand::Wait { target_pos } if target_pos == GridPosition { x: 4, y: 0 }),
            "移動済みの前衛より後方の射点へ追随し、同ターンの射撃を捏造しない: {command:?}"
        );
        *world.get_mut::<GridPosition>(gun).unwrap() = GridPosition { x: 4, y: 0 };
        world.get_mut::<HasMoved>(gun).unwrap().0 = true;
        assert!(decide_squad_action_v4(&mut world, player, &HashSet::default()).is_none());
        // 翌手番は再配置した射点から動かずに撃つ。
        world.get_mut::<HasMoved>(gun).unwrap().0 = false;
        let (actor, command) =
            decide_squad_action_v4(&mut world, player, &HashSet::default()).unwrap();
        assert_eq!(actor, gun);
        assert!(
            matches!(command, AiCommand::Attack { target_entity, target_pos }
            if target_entity == enemy && target_pos == GridPosition { x: 4, y: 0 })
        );
    }

    #[test]
    fn v4_squad_executor_defers_blocked_wait_until_front_squad_vacates() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(4, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map::new(4, 2, Terrain::Plains, GridTopology::Square));
        world.insert_resource(DamageChart::new());
        let mut stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        // 1マス先を塞がれると後方は同位置Waitになる一方、前方は確実に退去できる配置。
        stats.max_movement = 1;
        let rear = spawn_v3_test_unit(&mut world, player, 0, 100, stats.clone());
        let front = spawn_v3_test_unit(&mut world, player, 1, 100, stats.clone());
        let side = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(side).unwrap() = GridPosition { x: 0, y: 1 };
        let objective = GridPosition { x: 3, y: 0 };
        let mut manager = crate::ai::squad::SquadManager::new();
        let mut rear_squad =
            manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        rear_squad.members.insert(rear);
        rear_squad.plan.target = Some(objective);
        rear_squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        let mut front_squad =
            manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        front_squad.members.insert(front);
        front_squad.plan.target = Some(objective);
        front_squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        // 前方退去後にこの第三Squadへカーソルを進めず、保留した後方を先に再評価する。
        let mut side_squad =
            manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        side_squad.members.insert(side);
        side_squad.plan.target = Some(GridPosition { x: 3, y: 1 });
        side_squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        let (first_entity, first_command) =
            decide_squad_action_v4(&mut world, player, &HashSet::default())
                .expect("後方の同位置Waitより前方Squadの退去を先に選ぶ");
        assert_eq!(first_entity, front);
        let AiCommand::Wait {
            target_pos: front_target,
        } = first_command
        else {
            panic!("前方Squadは目的方向へ移動してWaitすること");
        };
        assert_eq!(front_target, GridPosition { x: 2, y: 0 });

        // 実行器が前方の移動を適用した次stepを再現する。保留した後方は未行動のまま。
        *world.get_mut::<GridPosition>(front).unwrap() = front_target;
        world.get_mut::<HasMoved>(front).unwrap().0 = true;
        world.get_mut::<ActionCompleted>(front).unwrap().0 = true;
        let skip_entities = HashSet::from_iter([front]);
        let (second_entity, second_command) =
            decide_squad_action_v4(&mut world, player, &skip_entities)
                .expect("出口が空いた後方Squadを同じ自手番で再評価する");
        assert_eq!(second_entity, rear);
        assert!(matches!(
            second_command,
            AiCommand::Wait {
                target_pos: GridPosition { x: 1, y: 0 }
            }
        ));
        assert!(!world.get::<HasMoved>(rear).unwrap().0);
        assert!(!world.get::<ActionCompleted>(rear).unwrap().0);
    }

    #[test]
    fn v4_squad_executor_commits_wait_after_one_fully_blocked_pass() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(2, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(DamageChart::new());
        let mut stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        stats.max_movement = 1;
        let left = spawn_v3_test_unit(&mut world, player, 0, 100, stats.clone());
        let right = spawn_v3_test_unit(&mut world, player, 1, 100, stats);
        let mut manager = crate::ai::squad::SquadManager::new();
        let mut left_squad =
            manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        left_squad.members.insert(left);
        left_squad.plan.target = Some(GridPosition { x: 1, y: 0 });
        left_squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        let mut right_squad =
            manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        right_squad.members.insert(right);
        right_squad.plan.target = Some(GridPosition { x: 0, y: 0 });
        right_squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        let (entity, command) = decide_squad_action_v4(&mut world, player, &HashSet::default())
            .expect("相互閉塞でも一巡後は最初のWaitを確定して終了する");

        assert_eq!(entity, left);
        assert!(matches!(
            command,
            AiCommand::Wait {
                target_pos: GridPosition { x: 0, y: 0 }
            }
        ));
    }

    #[test]
    fn map1_recon_can_move_and_attack_adjacent_infantry_on_hex_grid() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(2);
        let enemy_player = PlayerId(1);
        let mut world = setup_v3_test_world(10, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map::new(10, 14, Terrain::Plains, GridTopology::Hex));
        let registry = world.resource::<MasterDataRegistry>().clone();
        let recon_stats = registry
            .create_unit_stats(&UnitName(UnitType::Recon.as_str().to_owned()))
            .unwrap();
        let infantry_stats = registry
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let recon = world
            .spawn((
                Faction(player),
                GridPosition { x: 3, y: 10 },
                recon_stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: recon_stats.max_ammo1,
                    max_ammo1: recon_stats.max_ammo1,
                    ammo2: recon_stats.max_ammo2,
                    max_ammo2: recon_stats.max_ammo2,
                },
            ))
            .id();
        let infantry = world
            .spawn((
                Faction(enemy_player),
                GridPosition { x: 4, y: 6 },
                infantry_stats,
            ))
            .id();

        let snapshot = AiTacticalSnapshot::from_world(&mut world, &registry, GridTopology::Hex);
        let actions =
            snapshot.action_targets_at(recon, &recon_stats, GridPosition { x: 4, y: 7 }, true);

        assert!(
            actions.attackable_targets.contains(&infantry),
            "map_1のT2射撃位置では、移動後の装甲車が隣接歩兵を攻撃できる"
        );
    }

    #[test]
    fn damaged_unit_does_not_enter_factory_when_it_can_wait_off_site() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(3, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 3,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 0, y: 1 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(player), 100),
        ));
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        let infantry = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 0, y: 0 },
                stats.clone(),
                Health {
                    current: 50,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: stats.max_ammo1,
                    max_ammo1: stats.max_ammo1,
                    ammo2: stats.max_ammo2,
                    max_ammo2: stats.max_ammo2,
                },
                crate::components::Fuel {
                    current: stats.max_fuel,
                    max: stats.max_fuel,
                },
            ))
            .id();

        let (entity, command) =
            decide_ai_action_v2(&mut world, player, &HashSet::default()).expect("行動を選ぶこと");
        assert_eq!(entity, infantry);
        let AiCommand::Wait { target_pos } = command else {
            panic!("敵がいないためWaitを選ぶこと");
        };
        assert_ne!(
            target_pos,
            GridPosition { x: 1, y: 0 },
            "回復開始より次手番の生産slotを優先し、工場外で行動終了する"
        );
    }

    #[test]
    fn indirect_unit_fires_from_factory_when_other_production_slots_are_open() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let mut world = setup_v3_test_world(5, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 5,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 0, y: 1 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(player), 100),
        ));
        let artillery = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::LightSpGun.as_str().to_owned(),
            ))
            .unwrap();
        let unit = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                artillery.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: artillery.max_ammo1,
                    max_ammo1: artillery.max_ammo1,
                    ammo2: artillery.max_ammo2,
                    max_ammo2: artillery.max_ammo2,
                },
                crate::components::Fuel {
                    current: artillery.max_fuel,
                    max: artillery.max_fuel,
                },
            ))
            .id();
        let infantry = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        let target = world
            .spawn((
                Faction(enemy),
                GridPosition { x: 4, y: 0 },
                infantry,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let (entity, command) =
            decide_ai_action_v2(&mut world, player, &HashSet::default()).expect("行動を選ぶこと");
        assert_eq!(entity, unit);
        assert!(
            matches!(command, AiCommand::Attack { target_entity, target_pos }
            if target_entity == target && target_pos == GridPosition { x: 1, y: 0 }),
            "別の生産口が空いているとき、合法な間接射撃を工場退去で潰さない: {command:?}"
        );
    }

    #[test]
    fn v4_indirect_unit_vacates_factory_when_all_production_slots_are_blocked() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let mut world = setup_v3_test_world(5, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 5,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 0, y: 1 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(player), 100),
        ));
        let artillery = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::LightSpGun.as_str().to_owned(),
            ))
            .unwrap();
        let unit = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                artillery.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: artillery.max_ammo1,
                    max_ammo1: artillery.max_ammo1,
                    ammo2: artillery.max_ammo2,
                    max_ammo2: artillery.max_ammo2,
                },
                crate::components::Fuel {
                    current: artillery.max_fuel,
                    max: artillery.max_fuel,
                },
            ))
            .id();
        let infantry = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        // 首都（もう一つの生産口）を行動済みの味方が塞ぎ、全生産口が閉塞した状態にする。
        world.spawn((
            Faction(player),
            HasMoved(true),
            ActionCompleted(true),
            GridPosition { x: 0, y: 1 },
            infantry.clone(),
            Health {
                current: 100,
                max: 100,
            },
        ));
        // 工場上から射程内に入る敵。定位置射撃が可能な状況でも退去を選ぶかを確かめる。
        world.spawn((
            Faction(enemy),
            GridPosition { x: 4, y: 0 },
            infantry,
            Health {
                current: 100,
                max: 100,
            },
        ));

        let (entity, command) = decide_ai_action_v2_for_entities(
            &mut world,
            player,
            &HashSet::default(),
            Some(&HashSet::from_iter([unit])),
        )
        .expect("行動を選ぶこと");
        assert_eq!(entity, unit);
        // 定位置射撃の強制は、工場外で手番を終えられる工場上の間接ユニットには
        // 適用しない。全生産口が閉塞している間は生産口の回復を優先し、工場から退く。
        let destination = match command {
            AiCommand::Attack { target_pos, .. }
            | AiCommand::Wait { target_pos }
            | AiCommand::Capture { target_pos }
            | AiCommand::Merge { target_pos, .. } => target_pos,
            other => panic!("工場退去を伴う行動を選ぶこと: {other:?}"),
        };
        assert_ne!(
            destination,
            GridPosition { x: 1, y: 0 },
            "全生産口が閉塞しているとき、工場上の間接ユニットは射撃のために生産口を塞ぎ続けない: {command:?}"
        );
    }

    #[test]
    fn reference_map32_turn4_rocket_keeps_its_factory_firing_position() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 人間対局のP2第4手番開始時点。Entity番号は再生成し、座標・HP・弾薬を復元する。
        let snapshot: serde_json::Value =
            serde_json::from_str(include_str!("../../test_data/map32_human_turn4.json")).unwrap();
        let data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &data,
            "map_32",
            GridTopology::Hex,
        )
        .unwrap();
        let mut settings = crate::ai::PlayerAiSettings::new();
        settings.set_version(PlayerId(2), crate::ai::AiVersion::V4);
        world.insert_resource(settings);
        world
            .resource_mut::<crate::resources::MatchState>()
            .current_turn_number
            .0 = 4;
        for funds in snapshot["players_funds"].as_array().unwrap() {
            let id = PlayerId(funds[0].as_u64().unwrap() as u32);
            world
                .resource_mut::<crate::resources::Players>()
                .0
                .iter_mut()
                .find(|player| player.id == id)
                .unwrap()
                .funds = funds[1].as_u64().unwrap() as u32;
        }
        for property in snapshot["properties"].as_array().unwrap() {
            let at = GridPosition {
                x: property["x"].as_u64().unwrap() as usize,
                y: property["y"].as_u64().unwrap() as usize,
            };
            let owner = property["owner"]
                .as_u64()
                .map(|owner| PlayerId(owner as u32));
            let entity = world
                .iter_entities()
                .find(|entity| {
                    entity.get::<GridPosition>() == Some(&at) && entity.contains::<Property>()
                })
                .unwrap()
                .id();
            world.get_mut::<Property>(entity).unwrap().owner_id = owner;
        }
        let mut rocket = None;
        for source in snapshot["units"].as_array().unwrap() {
            let kind: UnitType = serde_json::from_value(source["unit_type"].clone()).unwrap();
            let stats = data
                .create_unit_stats(&UnitName(kind.as_str().to_owned()))
                .unwrap();
            let at = GridPosition {
                x: source["x"].as_u64().unwrap() as usize,
                y: source["y"].as_u64().unwrap() as usize,
            };
            let entity = world
                .spawn((
                    at,
                    Faction(PlayerId(source["player"].as_u64().unwrap() as u32)),
                    Health {
                        current: source["hp"].as_u64().unwrap() as u32,
                        max: 100,
                    },
                    crate::components::Fuel {
                        current: source["fuel"].as_u64().unwrap() as u32,
                        max: stats.max_fuel,
                    },
                    crate::components::Ammo {
                        ammo1: source["ammo1"].as_u64().unwrap() as u32,
                        ammo2: source["ammo2"].as_u64().unwrap() as u32,
                        max_ammo1: stats.max_ammo1,
                        max_ammo2: stats.max_ammo2,
                    },
                    HasMoved(false),
                    ActionCompleted(false),
                    stats,
                ))
                .id();
            if source["entity_index"] == 46 {
                rocket = Some(entity);
            }
        }
        let rocket = rocket.unwrap();
        let (_, command) = decide_ai_action_v2_for_entities(
            &mut world,
            PlayerId(2),
            &HashSet::default(),
            Some(&HashSet::from_iter([rocket])),
        )
        .unwrap();
        assert!(
            matches!(command, AiCommand::Attack { target_pos, .. }
            if target_pos == GridPosition { x: 7, y: 8 }),
            "工場上の射撃を残して侵入部隊を削る: {command:?}"
        );
    }

    fn setup_map42_human_turn5_world() -> (World, HashMap<u32, Entity>) {
        let snapshot: serde_json::Value =
            serde_json::from_str(include_str!("../../test_data/map42_human_turn5.json")).unwrap();
        let data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &data,
            "map_42",
            GridTopology::Hex,
        )
        .unwrap();

        let existing_units: Vec<Entity> = world
            .iter_entities()
            .filter(|e| e.contains::<UnitStats>())
            .map(|e| e.id())
            .collect();
        for entity in existing_units {
            world.despawn(entity);
        }

        let mut settings = crate::ai::PlayerAiSettings::new();
        settings.set_version(PlayerId(2), crate::ai::AiVersion::V4);
        world.insert_resource(settings);

        let p2_idx = world
            .resource::<crate::resources::Players>()
            .0
            .iter()
            .position(|p| p.id == PlayerId(2))
            .unwrap();
        {
            let mut match_state = world.resource_mut::<crate::resources::MatchState>();
            match_state.current_turn_number.0 = 5;
            match_state.active_player_index = crate::resources::PlayerIndex(p2_idx);
            match_state.current_phase = crate::resources::Phase::Main;
        }

        for funds in snapshot["players_funds"].as_array().unwrap() {
            let id = PlayerId(funds[0].as_u64().unwrap() as u32);
            world
                .resource_mut::<crate::resources::Players>()
                .0
                .iter_mut()
                .find(|player| player.id == id)
                .unwrap()
                .funds = funds[1].as_u64().unwrap() as u32;
        }

        for property in snapshot["properties"].as_array().unwrap() {
            let at = GridPosition {
                x: property["x"].as_u64().unwrap() as usize,
                y: property["y"].as_u64().unwrap() as usize,
            };
            let owner = property["owner"]
                .as_u64()
                .map(|owner| PlayerId(owner as u32));
            let entity = world
                .iter_entities()
                .find(|entity| {
                    entity.get::<GridPosition>() == Some(&at) && entity.contains::<Property>()
                })
                .unwrap()
                .id();
            world.get_mut::<Property>(entity).unwrap().owner_id = owner;
        }

        if let Some(captures) = snapshot
            .get("capture_progress_in_flight")
            .and_then(|c| c.as_array())
        {
            for cp in captures {
                let at = GridPosition {
                    x: cp["x"].as_u64().unwrap() as usize,
                    y: cp["y"].as_u64().unwrap() as usize,
                };
                let remaining = cp["remaining_capture_points"].as_u64().unwrap() as u32;
                let entity = world
                    .iter_entities()
                    .find(|entity| {
                        entity.get::<GridPosition>() == Some(&at) && entity.contains::<Property>()
                    })
                    .unwrap()
                    .id();
                world.get_mut::<Property>(entity).unwrap().capture_points = remaining;
            }
        }

        let mut entity_by_index = HashMap::default();
        for source in snapshot["units"].as_array().unwrap() {
            let kind: UnitType = serde_json::from_value(source["unit_type"].clone()).unwrap();
            let stats = data
                .create_unit_stats(&UnitName(kind.as_str().to_owned()))
                .unwrap();
            let at = GridPosition {
                x: source["x"].as_u64().unwrap() as usize,
                y: source["y"].as_u64().unwrap() as usize,
            };
            let has_moved = source
                .get("has_moved")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let action_completed = source
                .get("action_completed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let entity = world
                .spawn((
                    at,
                    Faction(PlayerId(source["player"].as_u64().unwrap() as u32)),
                    Health {
                        current: source["hp"].as_u64().unwrap() as u32,
                        max: 100,
                    },
                    crate::components::Fuel {
                        current: source["fuel"].as_u64().unwrap() as u32,
                        max: stats.max_fuel,
                    },
                    crate::components::Ammo {
                        ammo1: source["ammo1"].as_u64().unwrap() as u32,
                        ammo2: source["ammo2"].as_u64().unwrap() as u32,
                        max_ammo1: stats.max_ammo1,
                        max_ammo2: stats.max_ammo2,
                    },
                    HasMoved(has_moved),
                    ActionCompleted(action_completed),
                    stats,
                ))
                .id();
            let idx = source["entity_index"].as_u64().unwrap() as u32;
            entity_by_index.insert(idx, entity);
        }

        (world, entity_by_index)
    }

    #[test]
    fn reference_map42_turn5_heavyspgun_shoots_rockets_from_city() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 人間対局の知見: 自走砲は移動せず射程4の敵ロケット砲を安全に間接射撃すべき（コールドスタート再現、Squad履歴なし）
        let (mut world, entities) = setup_map42_human_turn5_world();
        let gun = entities[&77];
        let target_rocket = entities[&75];

        let (actor, command) = decide_ai_action_v2_for_entities(
            &mut world,
            PlayerId(2),
            &HashSet::default(),
            Some(&HashSet::from_iter([gun])),
        )
        .expect("自走砲の行動決定が得られること");

        assert_eq!(actor, gun, "自走砲自身が行動すること");
        assert!(
            matches!(command, AiCommand::Attack { target_entity, target_pos }
            if target_entity == target_rocket && target_pos == GridPosition { x: 3, y: 5 }),
            "自走砲(3,5)は移動せず(3,5)から敵ロケット砲(6,8)を攻撃すること。実際の結果: {command:?}"
        );
    }

    #[test]
    fn reference_map42_turn5_global_v4_first_command_is_heavyspgun_attack() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 全体V4実行でも自走砲による敵ロケット砲への間接射撃が初手として選ばれることを検証する
        let (mut world, entities) = setup_map42_human_turn5_world();
        let target_rocket = entities[&75];

        let first_command = execute_ai_turn_v2(&mut world, PlayerId(2));
        let expected_command = format!(
            "Attack {{ target_pos: GridPosition {{ x: 3, y: 5 }}, target_entity: {:?} }}",
            target_rocket
        );
        assert_eq!(
            first_command.as_deref(),
            Some(expected_command.as_str()),
            "全体V4初手で自走砲によるロケット砲への間接攻撃が実行されること"
        );
    }

    /// V3 テスト用の共通ワールドを構築するヘルパー。
    /// 幅 width x 高さ 1 の平原マップと必要リソースを登録する。
    fn setup_v3_test_world(width: usize, version: crate::ai::ai_version::AiVersion) -> World {
        let mut world = World::new();
        world.insert_resource(Map {
            width,
            height: 1,
            tiles: vec![Terrain::Plains; width],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();
        let mut settings = crate::ai::ai_version::PlayerAiSettings::new();
        settings.set_version(PlayerId(1), version);
        settings.set_version(PlayerId(2), version);
        world.insert_resource(settings);
        world
    }

    #[test]
    fn v2_v3_transport_executor_skips_foreign_owned_squads() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        for version in [
            crate::ai::ai_version::AiVersion::V2,
            crate::ai::ai_version::AiVersion::V3,
        ] {
            let mut world = setup_v3_test_world(5, version);
            let map = world.resource::<Map>().clone();
            world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
            world.insert_resource(Events::<crate::events::WaitUnitCommand>::default());
            world.insert_resource(Events::<crate::events::MoveUnitCommand>::default());
            let player_a = PlayerId(1);
            let player_b = PlayerId(2);
            let property_a = GridPosition { x: 1, y: 0 };
            let property_b = GridPosition { x: 3, y: 0 };
            world.spawn((
                property_a,
                Property::new(Terrain::City, Some(player_a), 100),
            ));
            world.spawn((
                property_b,
                Property::new(Terrain::City, Some(player_b), 100),
            ));
            let transport_stats = UnitStats {
                unit_type: UnitType::TransportHelicopter,
                movement_type: crate::resources::MovementType::Air,
                max_movement: 6,
                max_cargo: 2,
                ..UnitStats::mock()
            };
            let transport_a = world
                .spawn((
                    Faction(player_a),
                    GridPosition { x: 0, y: 0 },
                    transport_stats.clone(),
                    crate::components::Fuel {
                        current: 99,
                        max: 99,
                    },
                    crate::components::CargoCapacity {
                        max: 2,
                        loaded: Vec::new(),
                    },
                ))
                .id();
            let ownerless_transport = world
                .spawn((
                    Faction(player_b),
                    GridPosition { x: 2, y: 0 },
                    transport_stats.clone(),
                    crate::components::Fuel {
                        current: 99,
                        max: 99,
                    },
                    crate::components::CargoCapacity {
                        max: 2,
                        loaded: Vec::new(),
                    },
                ))
                .id();
            let transport_b = world
                .spawn((
                    Faction(player_b),
                    GridPosition { x: 4, y: 0 },
                    transport_stats,
                    crate::components::Fuel {
                        current: 99,
                        max: 99,
                    },
                    crate::components::CargoCapacity {
                        max: 2,
                        loaded: Vec::new(),
                    },
                ))
                .id();
            let (foreign_id, foreign_snapshot) = {
                let mut manager = crate::ai::squad::SquadManager::new();
                let mut foreign =
                    manager.create_owned_squad(crate::ai::squad::MissionType::Transport, player_a);
                foreign.members.insert(transport_a);
                foreign.transport_entity = Some(transport_a).into();
                foreign.plan.phase = crate::ai::squad::MissionPhase::Transport(
                    crate::ai::squad::TransportPhase::Return,
                );
                let snapshot = (
                    foreign.plan.owner_id,
                    foreign.members.clone(),
                    foreign.transport_entity,
                    foreign.cargo_entities.clone(),
                    foreign.delivered_cargo.clone(),
                    foreign.plan.target_island,
                    foreign.plan.target,
                    foreign.plan.phase.clone(),
                    foreign.plan.pickup_position,
                    foreign.plan.drop_position,
                );
                let id = foreign.id;
                let mut ownerless = manager.create_squad(crate::ai::squad::MissionType::Transport);
                ownerless.members.insert(ownerless_transport);
                ownerless.transport_entity = Some(ownerless_transport).into();
                ownerless.plan.phase = crate::ai::squad::MissionPhase::Transport(
                    crate::ai::squad::TransportPhase::Return,
                );
                let mut own =
                    manager.create_owned_squad(crate::ai::squad::MissionType::Transport, player_b);
                own.members.insert(transport_b);
                own.transport_entity = Some(transport_b).into();
                own.plan.phase = crate::ai::squad::MissionPhase::Transport(
                    crate::ai::squad::TransportPhase::Return,
                );
                world.insert_resource(manager);
                (id, snapshot)
            };
            let sentinel = world.spawn_empty().id();
            world.insert_resource(AiActionCooldown(HashSet::from_iter([sentinel])));

            let result_b = execute_ai_turn(&mut world, player_b);
            assert!(result_b.is_some());
            let manager = world.resource::<crate::ai::squad::SquadManager>();
            let foreign = manager
                .squads
                .iter()
                .find(|squad| squad.id == foreign_id)
                .unwrap();
            assert_eq!(
                (
                    foreign.plan.owner_id,
                    foreign.members.clone(),
                    foreign.transport_entity,
                    foreign.cargo_entities.clone(),
                    foreign.delivered_cargo.clone(),
                    foreign.plan.target_island,
                    foreign.plan.target,
                    foreign.plan.phase.clone(),
                    foreign.plan.pickup_position,
                    foreign.plan.drop_position,
                ),
                foreign_snapshot
            );
            let cooldown = world.resource::<AiActionCooldown>();
            assert!(!cooldown.0.contains(&transport_a));
            assert!(!cooldown.0.contains(&ownerless_transport));
            assert!(cooldown.0.contains(&transport_b));

            let result_a = execute_ai_turn(&mut world, player_a);
            assert!(result_a.is_some());
            assert!(
                world
                    .resource::<AiActionCooldown>()
                    .0
                    .contains(&transport_a)
            );
        }
    }

    /// 移動可能な自軍ユニットをスポーンするヘルパー
    fn spawn_v3_test_unit(
        world: &mut World,
        player: PlayerId,
        x: usize,
        hp: u32,
        stats: UnitStats,
    ) -> Entity {
        world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x, y: 0 },
                stats,
                Health {
                    current: hp,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id()
    }

    #[test]
    fn v3_combat_unit_does_not_blockade_enemy_production_site() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(2);
        let enemy = PlayerId(1);
        let mut world = setup_v3_test_world(6, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(Map {
            width: 6,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(enemy), 100),
        ));
        let enemy_stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        spawn_v3_test_unit(&mut world, enemy, 4, 100, enemy_stats);

        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Tank.as_str().to_owned(),
            ))
            .unwrap();
        let tank = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(tank).unwrap() = GridPosition { x: 0, y: 0 };

        let mut manager = crate::ai::squad::SquadManager::new();
        let mut squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        squad.members.insert(tank);
        squad.plan.target = Some(GridPosition { x: 4, y: 0 });
        squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        // V3思考では敵生産マスの封鎖評価を行わないため、敵工場 (1, 0) に留まらず
        // 敵首都 (4, 0) に向けて最遠到達点 (3, 0) まで前進する。
        let (entity, command) = decide_ai_action_v2(&mut world, player, &HashSet::default())
            .expect("Tank should take an action");
        assert_eq!(entity, tank);
        let AiCommand::Wait { target_pos } = command else {
            panic!("Wait行動を期待したが {:?} であった", command);
        };
        assert_eq!(
            target_pos,
            GridPosition { x: 3, y: 0 },
            "V3の戦車は封鎖評価を行わず、目標へ向かって前進すべき"
        );
    }

    #[test]
    fn v4_healthy_combat_unit_occupies_reachable_enemy_production_site_to_blockade_reinforcements()
    {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(2);
        let enemy = PlayerId(1);
        let mut world = setup_v3_test_world(6, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 6,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        // 敵 (P1) の首都 (4, 0) と工場 (1, 0) (距離3以内で生産可能範囲内)
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(enemy), 100),
        ));
        // 敵首都 (4, 0) に敵守備隊がいるため進入不可
        let enemy_stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        spawn_v3_test_unit(&mut world, enemy, 4, 100, enemy_stats);

        // 自軍 (P2) の戦車が (0, 0) にいる (工場 (1, 0) にも、より首都に近い平地 (2, 0) や (3, 0) にも移動可能)
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Tank.as_str().to_owned(),
            ))
            .unwrap();
        let tank = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(tank).unwrap() = GridPosition { x: 0, y: 0 };

        let mut manager = crate::ai::squad::SquadManager::new();
        let mut squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        squad.members.insert(tank);
        squad.plan.target = Some(GridPosition { x: 4, y: 0 }); // 敵首都を目標
        squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        let (entity, command) = decide_squad_action_v4(&mut world, player, &HashSet::default())
            .expect("Tank should take an action");
        assert_eq!(entity, tank);
        // 戦車は敵首都への単なる平地前進 (RouteAdvance) ではなく、敵の補充を物理的に封鎖できる工場 (1, 0) を占位してWaitすべき
        let AiCommand::Wait { target_pos } = command else {
            panic!("Wait行動を期待したが {:?} であった", command);
        };
        assert_eq!(
            target_pos,
            GridPosition { x: 1, y: 0 },
            "戦車は敵工場の生産を止めるために (1, 0) を占位すべき"
        );
    }

    #[test]
    fn v4_combat_unit_does_not_blockade_enemy_production_site_reserved_for_friendly_capture() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 占位（blockade）は敵の補充を止めるだけだが、占領（capture）は補充を止めたうえで
        // 収入と生産力を自軍へ移すため常に上位互換である。
        // 友軍のCapture契約が既にあるマスを戦闘ユニットが塞ぐのは純損失なので、
        // 占位は占領へ道を譲らなければならない。
        let player = PlayerId(2);
        let enemy = PlayerId(1);
        let mut world = setup_v3_test_world(6, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 6,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        // 敵 (P1) の首都 (4, 0) と工場 (1, 0)（生産可能範囲内）
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(enemy), 100),
        ));
        // 敵首都 (4, 0) に敵守備隊がいるため進入不可
        let enemy_stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        spawn_v3_test_unit(&mut world, enemy, 4, 100, enemy_stats);

        // 自軍 (P2) の戦車が (0, 0) にいる
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Tank.as_str().to_owned(),
            ))
            .unwrap();
        let tank = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(tank).unwrap() = GridPosition { x: 0, y: 0 };

        let mut manager = crate::ai::squad::SquadManager::new();
        let mut squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        squad.members.insert(tank);
        squad.plan.target = Some(GridPosition { x: 4, y: 0 }); // 敵首都を目標
        squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        // 友軍の占領部隊が敵工場 (1, 0) を占領目標として予約している。
        let mut capture_squad =
            manager.create_owned_squad(crate::ai::squad::MissionType::Capture, player);
        capture_squad.plan.target = Some(GridPosition { x: 1, y: 0 });
        world.insert_resource(manager);

        let (entity, command) = decide_squad_action_v4(&mut world, player, &HashSet::default())
            .expect("Tank should take an action");
        assert_eq!(entity, tank);
        let target_pos = match command {
            AiCommand::Wait { target_pos } => target_pos,
            AiCommand::Attack { target_pos, .. } => target_pos,
            AiCommand::Capture { target_pos } => target_pos,
            other => panic!("位置を伴う行動を期待したが {:?} であった", other),
        };
        assert_ne!(
            target_pos,
            GridPosition { x: 1, y: 0 },
            "友軍が占領予約している敵工場 (1, 0) を戦闘ユニットが占位して塞いではならない"
        );
    }

    #[test]
    fn v4_healthy_combat_unit_holds_enemy_production_site_blockade() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(2);
        let enemy = PlayerId(1);
        let mut world = setup_v3_test_world(4, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 4,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(enemy), 100),
        ));
        // 自軍 (P2) の戦車が既に工場 (1, 0) 上にいる
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Tank.as_str().to_owned(),
            ))
            .unwrap();
        let tank = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(tank).unwrap() = GridPosition { x: 1, y: 0 };

        let mut manager = crate::ai::squad::SquadManager::new();
        let mut squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        squad.members.insert(tank);
        // 目標は生産施設ではない平地 (0, 1)。敵首都 (3, 0) も占位対象なので、
        // そちらへ引かれる目標にすると「二つの占位マスのどちらを選ぶか」になってしまい、
        // 封鎖を維持するかどうかを検証できない。占位マスから引き離す目標を与える。
        squad.plan.target = Some(GridPosition { x: 0, y: 1 });
        squad.plan.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        let (entity, command) = decide_squad_action_v4(&mut world, player, &HashSet::default())
            .expect("Tank should take an action");
        assert_eq!(entity, tank);
        // 戦車は目標へ不用意に前進して工場を明け渡すのではなく、工場 (1, 0) で待機して封鎖を維持すべき
        let AiCommand::Wait { target_pos } = command else {
            panic!("Wait行動を期待したが {:?} であった", command);
        };
        assert_eq!(
            target_pos,
            GridPosition { x: 1, y: 0 },
            "戦車は敵工場 (1, 0) の封鎖を維持すべき"
        );
    }
}
