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
    if engine::ai::features::assault_go_enabled() {
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
            let action = execute_ai_turn(&mut world, active);
            schedule.run(&mut world);
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
                    "before":changed_before,"after":changed_after}),
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
        "assault_go_enabled": engine::ai::features::assault_go_enabled(),
        "outcome": outcome,
        "rounds": rounds,
        "action_trace": trace,
    });
    std::fs::write(out, serde_json::to_vec_pretty(&output).unwrap()).unwrap();
}
