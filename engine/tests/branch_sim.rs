//! 調査用（ignored）: 対局ログの途中の盤面から、両軍のAIで数手番だけ対局を進める分岐シミュレーション。
//!
//! 修正の効果を、60手番の対局を回す前に、問題の起きている局面だけで確かめるための道具。
//! 1. ログの `BRANCH_FROM - BRANCH_WARMUP` から `BRANCH_FROM - 1` までの各ラウンドの盤面で、
//!    対象のAIの手番の判断だけを再現し、作戦の割り当てなどの記憶を温める（盤面は毎回ログで上書き）。
//! 2. ラウンド `BRANCH_FROM - 1` の終わりの盤面と資金を置き、エンジンの手番開始処理（収入・補給・
//!    行動済みの解除）から、両軍のAIで `BRANCH_TURNS` ラウンドだけ対局を進める。
//! 3. 各ラウンドの終わりの盤面を、対局ログの `strategic_history` と同じ形で `BRANCH_OUT` に書く。
//!
//! 弾薬・燃料はログに無いので満タンで置く。輸送中の部隊は置かない。
//!
//! 使い方:
//! BRANCH_LOG=.../seed1.json BRANCH_FROM=10 BRANCH_TURNS=8 BRANCH_OUT=/tmp/x.json \
//! OPENWARS_CONCENTRATE=1 ... cargo test --release -p engine --test branch_sim -- --ignored --nocapture
use bevy_ecs::prelude::*;
use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::ai::execute_ai_turn;
use engine::components::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;
use std::collections::{HashMap, HashSet};

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// ログの1ラウンドの盤面を置く。同じ部隊（ログのunit_id）は同じEntityのまま更新する。
fn place_snapshot(
    world: &mut World,
    md: &MasterDataRegistry,
    snapshot: &serde_json::Value,
    entities: &mut HashMap<u64, Entity>,
) {
    let seen = snapshot["units"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|unit| unit["transporting_by"].is_null())
        .map(|unit| unit["unit_id"].as_u64().unwrap())
        .collect::<HashSet<_>>();
    entities.retain(|id, _| seen.contains(id));
    let strays = world
        .query_filtered::<Entity, With<Faction>>()
        .iter(world)
        .filter(|entity| !entities.values().any(|known| known == entity))
        .collect::<Vec<_>>();
    for entity in strays {
        world.despawn(entity);
    }
    let owners = snapshot["properties"]
        .as_array()
        .unwrap()
        .iter()
        .map(|property| {
            (
                (
                    property["x"].as_u64().unwrap() as usize,
                    property["y"].as_u64().unwrap() as usize,
                ),
                (
                    property["owner"]
                        .as_u64()
                        .map(|owner| PlayerId(owner as u32)),
                    property["capture_points"].as_u64().unwrap_or(0) as u32,
                ),
            )
        })
        .collect::<HashMap<_, _>>();
    for (position, mut property) in world
        .query::<(&GridPosition, &mut Property)>()
        .iter_mut(world)
    {
        if let Some((owner, points)) = owners.get(&(position.x, position.y)) {
            property.owner_id = *owner;
            property.capture_points = *points;
        }
    }
    for unit in snapshot["units"].as_array().unwrap() {
        if !unit["transporting_by"].is_null() {
            continue;
        }
        let unit_type = md
            .unit_type_for_name(unit["unit_type"].as_str().unwrap())
            .unwrap();
        let stats = world
            .resource::<UnitRegistry>()
            .get_stats(unit_type)
            .unwrap()
            .clone();
        let owner = PlayerId(unit["player_id"].as_u64().unwrap() as u32);
        let id = unit["unit_id"].as_u64().unwrap();
        let fresh = (
            GridPosition {
                x: unit["x"].as_u64().unwrap() as usize,
                y: unit["y"].as_u64().unwrap() as usize,
            },
            Health {
                current: unit["hp"].as_u64().unwrap() as u32,
                max: 100,
            },
            Fuel {
                current: stats.max_fuel,
                max: stats.max_fuel,
            },
            Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            },
            HasMoved(false),
            ActionCompleted(false),
        );
        if let Some(&entity) = entities.get(&id)
            && world.get_entity(entity).is_ok()
        {
            world.entity_mut(entity).insert(fresh);
            continue;
        }
        let entity = world
            .spawn((UnitId(uuid::Uuid::new_v4()), Faction(owner), stats.clone()))
            .insert(fresh)
            .id();
        if stats.max_cargo > 0 {
            world.entity_mut(entity).insert(CargoCapacity {
                max: stats.max_cargo,
                loaded: vec![],
            });
        }
        entities.insert(id, entity);
    }
}

/// ラウンドの終わりの資金（`metrics` の `pN_funds`）を置く。
fn place_funds(world: &mut World, metrics: &[serde_json::Value], round: u32) {
    let Some(metric) = metrics
        .iter()
        .find(|metric| metric["turn"].as_u64() == Some(u64::from(round)))
    else {
        return;
    };
    let mut players = world.resource_mut::<Players>();
    for player in players.0.iter_mut() {
        let key = format!("p{}_funds", player.id.0);
        if let Some(funds) = metric[key.as_str()].as_u64() {
            player.funds = funds as u32;
        }
    }
}

/// 盤面を対局ログの `strategic_history` と同じ形で書き出す（兵種名はマスターデータの名前）。
fn snapshot_json(world: &mut World, md: &MasterDataRegistry, round: u32) -> serde_json::Value {
    let name_of = |unit_type: UnitType| {
        md.units
            .keys()
            .find(|name| md.unit_type_for_name(&name.0).ok() == Some(unit_type))
            .map(|name| name.0.clone())
            .unwrap_or_default()
    };
    let transporting = world
        .query::<(Entity, &Transporting)>()
        .iter(world)
        .map(|(entity, transporting)| (transporting.0, entity))
        .collect::<HashMap<_, _>>();
    let units = world
        .query::<(Entity, &GridPosition, &Faction, &UnitStats, &Health)>()
        .iter(world)
        .map(|(entity, position, faction, stats, health)| {
            serde_json::json!({
                "unit_id": entity.to_bits(),
                "player_id": faction.0.0,
                "unit_type": name_of(stats.unit_type),
                "x": position.x,
                "y": position.y,
                "hp": health.current,
                "cost": stats.cost,
                "transporting_by": transporting.get(&entity).map(|carrier| carrier.to_bits()),
            })
        })
        .collect::<Vec<_>>();
    let properties = world
        .query::<(&GridPosition, &Property)>()
        .iter(world)
        .map(|(position, property)| {
            serde_json::json!({
                "x": position.x,
                "y": position.y,
                "owner": property.owner_id.map(|owner| owner.0),
                "terrain": format!("{:?}", property.terrain),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({"turn": round, "units": units, "properties": properties})
}

#[test]
#[ignore]
fn branch_sim() {
    let path = std::env::var("BRANCH_LOG").expect("BRANCH_LOG");
    let from = env_u32("BRANCH_FROM", 10);
    let turns = env_u32("BRANCH_TURNS", 8);
    let warmup = env_u32("BRANCH_WARMUP", 3);
    let map_name = std::env::var("BRANCH_MAP").unwrap_or_else(|_| "map_20".into());
    let subject = PlayerId(env_u32("BRANCH_PLAYER", 2));
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
    world.insert_resource(GameRng::new(u64::from(env_u32("BRANCH_SEED", 1))));
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
        place_snapshot(&mut world, &md, snapshot, &mut entities);
        place_funds(&mut world, &metrics, round);
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
    place_snapshot(&mut world, &md, start, &mut entities);
    place_funds(&mut world, &metrics, from - 1);
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

    let mut rounds = vec![snapshot_json(&mut world, &md, from - 1)];
    loop {
        let (turn, active, over) = {
            let state = world.resource::<MatchState>();
            let players = world.resource::<Players>();
            (
                state.current_turn_number.0,
                players.0[state.active_player_index.0].id,
                state.game_over.is_some(),
            )
        };
        if over || turn >= from + turns {
            break;
        }
        for _ in 0..400 {
            let action = execute_ai_turn(&mut world, active);
            schedule.run(&mut world);
            if action.is_none() {
                break;
            }
        }
        let last = world.resource::<Players>().0.last().unwrap().id;
        if active == last {
            rounds.push(snapshot_json(&mut world, &md, turn));
            eprintln!("BRANCH round {turn} done");
        }
    }
    let output = serde_json::json!({"results": [{"strategic_history": rounds, "from": from, "turns": turns}]});
    std::fs::write(&out, serde_json::to_string(&output).unwrap()).unwrap();
}
