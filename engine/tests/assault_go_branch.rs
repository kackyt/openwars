//! 調査用（ignored）: 対局ログの途中の盤面から、決着か最終手番まで両軍のAIで対局を進める。
//!
//! 攻勢（GO）処理の効果を、GOが出た局面から再開して通常勝利へ持ち込めるかで確かめる。
//! 同じ局面から `OPENWARS_ASSAULT_GO` の有無で走らせ、結果を比べる。
//! 1. `BRANCH_FROM - BRANCH_WARMUP - 1` から `BRANCH_FROM - 2` の盤面で、対象AIの手番の判断だけを
//!    再現して記憶を温める（盤面は毎回ログで上書き）。
//! 2. ラウンド `BRANCH_FROM - 1` の終わりの盤面と資金を置き、手番開始処理から対局を進める。
//! 3. 決着か `BRANCH_MAX_TURN` 手番の終了までの結果と、各ラウンドの要約を `BRANCH_OUT` に書く。
//!
//! 弾薬・燃料はログに無いので満タンで置く。
#[path = "branch_sim.rs"]
mod branch_fixture;

use bevy_ecs::prelude::*;
use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::ai::execute_ai_turn;
use engine::components::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;
use std::collections::HashMap;

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// ラウンド終わりの要約（勢力ごとの物件数・部隊価値、敵首都の占領残り）。
fn round_summary(world: &mut World, round: u32, subject: PlayerId) -> serde_json::Value {
    let mut properties = HashMap::<u32, u32>::new();
    let mut capital = None;
    for (position, property) in world.query::<(&GridPosition, &Property)>().iter(world) {
        if let Some(owner) = property.owner_id {
            *properties.entry(owner.0).or_default() += 1;
            if property.terrain == Terrain::Capital && owner != subject {
                capital = Some((position.x, position.y, property.capture_points));
            }
        }
    }
    let mut value = HashMap::<u32, u64>::new();
    for (faction, stats, health) in world.query::<(&Faction, &UnitStats, &Health)>().iter(world) {
        *value.entry(faction.0.0).or_default() +=
            u64::from(stats.cost) * u64::from(health.current) / 100;
    }
    let go = engine::ai::assault_decision::assault_objective(world, subject).is_some();
    serde_json::json!({
        "round": round,
        "properties": properties,
        "unit_value": value,
        "enemy_capital": capital,
        "assault_go": go,
    })
}

/// 行動前後を照合するための部隊状態。位置・HPに加え行動完了も記録する。
fn trace_units(world: &mut World) -> Vec<(u64, u32, String, usize, usize, u32, bool)> {
    let mut units = world
        .query::<(
            Entity,
            &Faction,
            &UnitStats,
            &GridPosition,
            &Health,
            &ActionCompleted,
        )>()
        .iter(world)
        .map(|(entity, faction, stats, pos, hp, done)| {
            (
                entity.to_bits(),
                faction.0.0,
                format!("{:?}", stats.unit_type),
                pos.x,
                pos.y,
                hp.current,
                done.0,
            )
        })
        .collect::<Vec<_>>();
    units.sort_by_key(|unit| unit.0);
    units
}

/// 診断用の初手介入。実際のAIへ固定座標のルールを追加しない。
#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct OpeningOrder {
    from: [usize; 2],
    to: Option<[usize; 2]>,
    target: Option<[usize; 2]>,
}

fn execute_opening(
    world: &mut World,
    schedule: &mut Schedule,
    player: PlayerId,
    order: &OpeningOrder,
) -> String {
    use engine::ai::AiCommand;
    use engine::systems::movement::{OccupantInfo, calculate_reachable_tiles};
    let from = GridPosition {
        x: order.from[0],
        y: order.from[1],
    };
    let actor = world
        .iter_entities()
        .find_map(|e| {
            (e.get::<Faction>().is_some_and(|f| f.0 == player)
                && e.get::<GridPosition>() == Some(&from)
                && e.get::<UnitStats>().is_some()
                && e.get::<Transporting>().is_none())
            .then_some(e.id())
        })
        .expect("介入する部隊が指定位置にいない");
    assert!(engine::systems::action::is_unit_selectable(world, actor));
    let to = order.to.map_or(from, |p| GridPosition { x: p[0], y: p[1] });
    let stats = world.get::<UnitStats>(actor).unwrap().clone();
    let occupied = world
        .iter_entities()
        .filter_map(|e| {
            if e.get::<Transporting>().is_some() {
                return None;
            }
            let p = e.get::<GridPosition>()?;
            let f = e.get::<Faction>()?;
            let s = e.get::<UnitStats>()?;
            let c = e.get::<CargoCapacity>();
            Some((
                (p.x, p.y),
                OccupantInfo {
                    player_id: f.0,
                    is_transport: s.max_cargo > 0,
                    unit_type: s.unit_type,
                    loadable_types: s.loadable_unit_types.clone(),
                    free_slots: c
                        .map_or(s.max_cargo, |c| c.max.saturating_sub(c.loaded.len() as u32)),
                },
            ))
        })
        .collect();
    let reachable = calculate_reachable_tiles(
        world.resource::<Map>(),
        &occupied,
        (from.x, from.y),
        stats.movement_type,
        stats.max_movement,
        world.get::<Fuel>(actor).unwrap().current,
        player,
        stats.unit_type,
        world.resource::<MasterDataRegistry>(),
    );
    assert!(
        reachable.contains(&(to.x, to.y)),
        "指定先へ合法に移動できない: {from:?} -> {to:?}"
    );
    let mut expected_hit = None;
    let command = if let Some(p) = order.target {
        let target_pos = GridPosition { x: p[0], y: p[1] };
        let target = world
            .iter_entities()
            .find_map(|e| {
                (e.get::<Faction>().is_some_and(|f| f.0 != player)
                    && e.get::<GridPosition>() == Some(&target_pos)
                    && e.get::<UnitStats>().is_some())
                .then_some(e.id())
            })
            .expect("指定した攻撃対象がいない");
        assert!(
            engine::systems::combat::get_attackable_targets_at(world, actor, to, to == from)
                .contains(&target),
            "指定対象を合法に攻撃できない"
        );
        expected_hit = Some((target, world.get::<Health>(target).unwrap().current));
        AiCommand::Attack {
            target_pos: to,
            target_entity: target,
        }
    } else {
        assert!(
            to == from || !occupied.contains_key(&(to.x, to.y)),
            "介入先が占有されている"
        );
        let capture = stats.can_capture
            && world.iter_entities().any(|e| {
                e.get::<GridPosition>() == Some(&to)
                    && e.get::<Property>()
                        .is_some_and(|p| p.owner_id != Some(player))
            });
        if capture {
            AiCommand::Capture { target_pos: to }
        } else {
            AiCommand::Wait { target_pos: to }
        }
    };
    let label = format!("{command:?}");
    engine::ai::execute_ai_command(world, actor, command);
    schedule.run(world);
    assert_eq!(
        world.get::<GridPosition>(actor),
        Some(&to),
        "介入の移動命令が成立しなかった"
    );
    assert!(
        world.get::<ActionCompleted>(actor).is_some_and(|d| d.0),
        "介入が行動完了になっていない"
    );
    if let Some((target, hp)) = expected_hit {
        assert!(
            world.get::<Health>(target).is_none_or(|h| h.current < hp),
            "介入の攻撃が成立しなかった"
        );
    }
    label
}

/// 継続前進の診断。固定砲台・輸送役・占領役には介入せず、自力移動できる戦闘車両を
/// 前方から順に動かす。射撃可能部隊も動かす案では、移動後に敵を射程に収めるセルだけを使う。
fn front_push_order(
    world: &mut World,
    player: PlayerId,
    include_firing: bool,
) -> Option<OpeningOrder> {
    use engine::ai::turn_distance::{TurnDistanceCache, calculate_turn_distance};
    use engine::systems::movement::{OccupantInfo, calculate_reachable_tiles};
    let objective = engine::ai::assault_decision::assault_objective(world, player)?;
    let units = world
        .iter_entities()
        .filter_map(|e| {
            let stats = e.get::<UnitStats>()?;
            let pos = *e.get::<GridPosition>()?;
            if e.get::<Faction>()?.0 != player
                || e.get::<Transporting>().is_some()
                || stats.can_capture
                || stats.max_range == 0
                || stats.max_movement <= 1
                || matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
                || !engine::systems::action::is_unit_selectable(world, e.id())
                || e.get::<HasMoved>()?.0
                || e.get::<Ammo>()
                    .is_some_and(|a| a.ammo1 == 0 && a.ammo2 == 0)
            {
                return None;
            }
            Some((e.id(), pos, stats.clone(), e.get::<Fuel>()?.current))
        })
        .collect::<Vec<_>>();
    let occupied = world
        .iter_entities()
        .filter_map(|e| {
            if e.get::<Transporting>().is_some() {
                return None;
            }
            let p = e.get::<GridPosition>()?;
            let f = e.get::<Faction>()?;
            let s = e.get::<UnitStats>()?;
            Some((
                (p.x, p.y),
                OccupantInfo {
                    player_id: f.0,
                    is_transport: s.max_cargo > 0,
                    unit_type: s.unit_type,
                    loadable_types: s.loadable_unit_types.clone(),
                    free_slots: 0,
                },
            ))
        })
        .collect();
    let empty = Default::default();
    let mut cache = TurnDistanceCache::default();
    let mut best = None;
    for (actor, from, stats, fuel) in units {
        let current_targets =
            engine::systems::combat::get_attackable_targets_at(world, actor, from, true);
        let has_shot = current_targets.iter().any(|&target| {
            engine::ai::pruning::evaluate_attack_exchange(
                world,
                actor,
                target,
                from,
                world.resource::<DamageChart>(),
            )
            .is_some_and(|e| e.expected_damage > 0)
        });
        if has_shot && !include_firing {
            continue;
        }
        // 地形だけの残り経路で前進方向を決める。敵・味方の占有は実際の移動候補生成で守る。
        let range = if stats.min_range > 1 {
            stats.min_range
        } else {
            0
        };
        let before = calculate_turn_distance(
            world.resource::<Map>(),
            world.resource::<MasterDataRegistry>(),
            &empty,
            (from.x, from.y),
            (objective.x, objective.y),
            stats.movement_type,
            stats.max_movement,
            range,
            player,
            &mut cache,
        );
        if before.used_mp == u32::MAX {
            continue;
        }
        let reachable = calculate_reachable_tiles(
            world.resource::<Map>(),
            &occupied,
            (from.x, from.y),
            stats.movement_type,
            stats.max_movement,
            fuel,
            player,
            stats.unit_type,
            world.resource::<MasterDataRegistry>(),
        );
        for (x, y) in reachable {
            let to = GridPosition { x, y };
            if to == from || occupied.contains_key(&(x, y)) {
                continue;
            }
            let after = calculate_turn_distance(
                world.resource::<Map>(),
                world.resource::<MasterDataRegistry>(),
                &empty,
                (x, y),
                (objective.x, objective.y),
                stats.movement_type,
                stats.max_movement,
                range,
                player,
                &mut cache,
            );
            if after.used_mp >= before.used_mp {
                continue;
            }
            // 直接部隊は今手番の移動攻撃、間接部隊は移動後の次手番の射撃位置を調べる。
            let targets = engine::systems::combat::get_attackable_targets_at(
                world,
                actor,
                to,
                stats.min_range > 1,
            );
            let mut attack = None;
            for target in targets {
                let Some(exchange) = engine::ai::pruning::evaluate_attack_exchange(
                    world,
                    actor,
                    target,
                    to,
                    world.resource::<DamageChart>(),
                ) else {
                    continue;
                };
                if exchange.expected_damage == 0 {
                    continue;
                }
                let position = *world.get::<GridPosition>(target).unwrap();
                let hp = world.get::<Health>(actor).unwrap().current;
                if stats.min_range <= 1 && exchange.expected_counter_damage < hp {
                    let rank = (
                        exchange.expected_damage,
                        std::cmp::Reverse(world.resource::<Map>().distance(
                            position.x,
                            position.y,
                            objective.x,
                            objective.y,
                        )),
                    );
                    if attack.as_ref().is_none_or(|(old, _)| rank > *old) {
                        attack = Some((rank, position));
                    }
                }
            }
            if has_shot
                && stats.min_range > 1
                && !engine::systems::combat::get_attackable_targets_at(world, actor, to, true)
                    .iter()
                    .any(|&target| {
                        engine::ai::pruning::evaluate_attack_exchange(
                            world,
                            actor,
                            target,
                            to,
                            world.resource::<DamageChart>(),
                        )
                        .is_some_and(|e| e.expected_damage > 0)
                    })
            {
                continue;
            }
            let rank = (before.used_mp, after.used_mp, y, x, actor.to_bits());
            if best.as_ref().is_none_or(|(old, _)| rank < *old) {
                best = Some((
                    rank,
                    OpeningOrder {
                        from: [from.x, from.y],
                        to: Some([x, y]),
                        target: attack.map(|(_, p)| [p.x, p.y]),
                    },
                ));
            }
        }
    }
    best.map(|(_, order)| order)
}

#[test]
#[ignore]
fn assault_go_branch() {
    let path = std::env::var("BRANCH_LOG").expect("BRANCH_LOG");
    let from = env_u32("BRANCH_FROM", 10);
    let warmup = env_u32("BRANCH_WARMUP", 3);
    let max_turn = env_u32("BRANCH_MAX_TURN", 60);
    let map_name = std::env::var("BRANCH_MAP").expect("BRANCH_MAP");
    let subject = PlayerId(env_u32("BRANCH_PLAYER", 1));
    let out = std::env::var("BRANCH_OUT").expect("BRANCH_OUT");
    let log: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let result = &log["results"][0];
    let history = result["strategic_history"].as_array().unwrap();
    let metrics = result["metrics"].as_array().unwrap().clone();
    let snapshot_at = |round: u32| {
        history
            .iter()
            .find(|snapshot| snapshot["turn"].as_u64() == Some(u64::from(round)))
    };

    let md = MasterDataRegistry::load().unwrap();
    let (mut world, mut schedule) = engine::setup::initialize_world_from_master_data_with_topology(
        &md,
        &map_name,
        GridTopology::Hex,
    )
    .unwrap();
    world.insert_resource(GameRng::new(result["seed"].as_u64().unwrap_or(1)));
    let mut settings = PlayerAiSettings::default();
    for id in [PlayerId(1), PlayerId(2)] {
        settings.set_version(
            id,
            if id == subject {
                AiVersion::V4
            } else {
                AiVersion::V200
            },
        );
    }
    world.insert_resource(settings);
    world.insert_resource(engine::ai::wait_probe::WaitProbe::default());
    let subject_index = world
        .resource::<Players>()
        .0
        .iter()
        .position(|player| player.id == subject)
        .unwrap();
    let mut entities = HashMap::<u64, Entity>::new();

    // 1. 記憶を温める: 対象のAIの手番の判断だけを、ログの盤面で再現する。
    for round in from.saturating_sub(warmup + 1)..from.saturating_sub(1) {
        let Some(snapshot) = snapshot_at(round) else {
            continue;
        };
        branch_fixture::place_snapshot(&mut world, &md, snapshot, &mut entities);
        branch_fixture::place_funds(&mut world, &metrics, round);
        {
            let mut state = world.resource_mut::<MatchState>();
            state.current_turn_number = TurnNumber(round + 1);
            state.active_player_index = PlayerIndex(subject_index);
            state.current_phase = Phase::Main;
            state.game_over = None;
        }
        for _ in 0..400 {
            let action = execute_ai_turn(&mut world, subject);
            schedule.run(&mut world);
            if action.is_none() {
                break;
            }
        }
    }

    // 2. ラウンド from-1 の終わりの盤面から、両軍で対局を進める。
    let start = snapshot_at(from - 1).expect("出発点の盤面がログにない");
    branch_fixture::place_snapshot(&mut world, &md, start, &mut entities);
    branch_fixture::place_funds(&mut world, &metrics, from - 1);
    // GOが出た元のラウンド末盤面で判定を確定してから手番を開始する。
    // 特に後手は、先手の行動後に初めて判定すると元のGO局面を失ってしまう。
    world.insert_resource(engine::ai::assault_decision::AssaultPostures::default());
    let assault_go_enabled = std::env::var("OPENWARS_ASSAULT_GO").is_ok();
    if assault_go_enabled {
        engine::ai::assault_decision::refresh_assault_posture(&mut world, subject, from - 1);
        assert!(
            engine::ai::assault_decision::assault_objective(&world, subject).is_some(),
            "再開元の盤面でGOが成立していない"
        );
    }
    {
        let last = world.resource::<Players>().0.len() - 1;
        let mut state = world.resource_mut::<MatchState>();
        state.current_turn_number = TurnNumber(from - 1);
        state.active_player_index = PlayerIndex(last);
        state.current_phase = Phase::EndTurn;
        state.game_over = None;
    }
    engine::systems::turn_management::advance_next_phase(&mut world);
    schedule.run(&mut world);

    let opening: Vec<OpeningOrder> = std::env::var("BRANCH_OPENING_FILE")
        .ok()
        .map_or_else(Vec::new, |p| {
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
        });
    let mut opening_index = 0;
    let push_mode = std::env::var("BRANCH_FRONT_PUSH").ok();
    assert!(
        push_mode
            .as_deref()
            .is_none_or(|mode| mode == "idle" || mode == "all")
    );
    let mut push_count = 0;
    let detailed = std::env::var_os("BRANCH_TRACE_ACTIONS").is_some();
    let mut trace = Vec::new();
    if detailed {
        trace.push(serde_json::json!({"initial": trace_units(&mut world)}));
    }
    let mut rounds = Vec::new();
    let mut last_round = from - 1;
    let outcome = loop {
        let (turn, active, over) = {
            let state = world.resource::<MatchState>();
            let players = world.resource::<Players>();
            (
                state.current_turn_number.0,
                players.0[state.active_player_index.0].id,
                state.game_over.clone(),
            )
        };
        if let Some(condition) = over {
            break match condition {
                GameOverCondition::Winner(winner) => serde_json::json!({
                    "type": "normal", "winner": winner.0, "turn": turn,
                }),
                GameOverCondition::Draw => serde_json::json!({"type": "draw", "turn": turn}),
            };
        }
        // ラウンドが進んだら、前のラウンドの終わりの要約を残す。
        if turn != last_round && active == PlayerId(1) && turn > from {
            rounds.push(round_summary(&mut world, turn - 1, subject));
            last_round = turn;
        }
        if turn > max_turn {
            break serde_json::json!({"type": "max_turns", "turn": max_turn});
        }
        let mut finished = false;
        for _ in 0..2000 {
            let before = if detailed {
                trace_units(&mut world)
            } else {
                Vec::new()
            };
            let mut pushed = false;
            if active == subject && push_mode.is_some() {
                engine::ai::assault_decision::refresh_assault_posture(&mut world, subject, turn);
            }
            let order = if active == subject
                && push_mode.is_some()
                && !(turn == from && opening_index < opening.len())
            {
                front_push_order(&mut world, subject, push_mode.as_deref() == Some("all"))
            } else {
                None
            };
            let action = if active == subject && turn == from && opening_index < opening.len() {
                engine::ai::assault_decision::refresh_assault_posture(&mut world, subject, turn);
                assert!(
                    engine::ai::assault_decision::assault_objective(&world, subject).is_some(),
                    "介入時にGOが成立していない"
                );
                let label =
                    execute_opening(&mut world, &mut schedule, subject, &opening[opening_index]);
                opening_index += 1;
                Some(label)
            } else if let Some(order) = order {
                pushed = true;
                push_count += 1;
                Some(execute_opening(&mut world, &mut schedule, subject, &order))
            } else {
                let action = execute_ai_turn(&mut world, active);
                schedule.run(&mut world);
                action
            };
            if detailed {
                let after = trace_units(&mut world);
                let changed_before = before
                    .iter()
                    .filter(|u| !after.contains(u))
                    .collect::<Vec<_>>();
                let changed_after = after
                    .iter()
                    .filter(|u| !before.contains(u))
                    .collect::<Vec<_>>();
                trace.push(
                    serde_json::json!({"turn":turn,"player":active.0,"action":action,
                    "go":engine::ai::assault_decision::assault_objective(&world, subject).is_some(),
                    "front_push":pushed,"before":changed_before,"after":changed_after}),
                );
            }
            if action.is_none() {
                finished = true;
                break;
            }
        }
        assert!(finished, "1手番が2000命令で終わらない");
    };
    rounds.push(round_summary(&mut world, last_round, subject));
    let output = serde_json::json!({
        "from": from,
        "subject": subject.0,
        "assault_go_enabled": assault_go_enabled,
        "outcome": outcome,
        "opening_orders": opening,
        "opening_executed": opening_index,
        "push_mode": push_mode,
        "push_count": push_count,
        "rounds": rounds,
        "action_trace": trace,
    });
    std::fs::write(out, serde_json::to_vec_pretty(&output).unwrap()).unwrap();
}
