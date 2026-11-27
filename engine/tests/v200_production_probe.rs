//! 調査用（ignored・未コミット）: 対局ログのラウンド `PROBE_FROM - 1` の終わりの盤面から、
//! P1（V200）の手番だけを進め、生産した部隊数を数える。
//!
//! 盤面の置き方は `branch_sim.rs` と同じ（弾薬・燃料は満タン）。
//! `PROBE_REMOVE_P2_WITHIN=k` を付けると、P1 首都から距離 k 以内の P2 部隊を取り除いてから進める
//! （首都近くへの侵入が V200 の生産に効くかを見るための切除実験）。
//!
//! 出力: 標準出力に `PROBE {"log":..., "from":..., "produced":[[x,y,"兵種"],...]}`
#[path = "branch_sim.rs"]
mod branch_sim;

use bevy_ecs::prelude::*;
use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::ai::execute_ai_turn;
use engine::components::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;
use std::collections::{HashMap, HashSet};

fn env_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

#[test]
#[ignore]
fn v200_production_probe() {
    let path = std::env::var("PROBE_LOG").expect("PROBE_LOG");
    let from = env_u32("PROBE_FROM").expect("PROBE_FROM");
    let remove_within = env_u32("PROBE_REMOVE_P2_WITHIN");
    let log: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let result = &log["results"][0];
    let history = result["strategic_history"].as_array().unwrap();
    let metrics = result["metrics"].as_array().unwrap().clone();
    let start = history
        .iter()
        .find(|s| s["turn"].as_u64() == Some(u64::from(from - 1)))
        .expect("出発点の盤面がログにない");

    let md = MasterDataRegistry::load().unwrap();
    let (mut world, mut schedule) = engine::setup::initialize_world_from_master_data_with_topology(
        &md,
        "map_42",
        GridTopology::Hex,
    )
    .unwrap();
    world.insert_resource(GameRng::new(1));
    let mut settings = PlayerAiSettings::default();
    settings.set_version(PlayerId(1), AiVersion::V200);
    settings.set_version(PlayerId(2), AiVersion::V4);
    world.insert_resource(settings);

    let mut entities = HashMap::<u64, Entity>::new();
    // V200 の手番内の記憶（直前の生産戦略・カウンタ）を温める: 直前 `PROBE_WARMUP` ラウンドの
    // 盤面で P1 の手番を再現する（盤面は毎回ログで上書きするので結果は捨てる）
    let warmup = env_u32("PROBE_WARMUP").unwrap_or(2);
    for round in from.saturating_sub(warmup + 2)..from.saturating_sub(1) {
        let Some(snapshot) = history
            .iter()
            .find(|s| s["turn"].as_u64() == Some(u64::from(round)))
        else {
            continue;
        };
        branch_sim::place_snapshot(&mut world, &md, snapshot, &mut entities);
        branch_sim::place_funds(&mut world, &metrics, round);
        {
            let last = world.resource::<Players>().0.len() - 1;
            let mut state = world.resource_mut::<MatchState>();
            state.current_turn_number = TurnNumber(round);
            state.active_player_index = PlayerIndex(last);
            state.current_phase = Phase::EndTurn;
            state.game_over = None;
        }
        engine::systems::turn_management::advance_next_phase(&mut world);
        schedule.run(&mut world);
        for _ in 0..400 {
            let action = execute_ai_turn(&mut world, PlayerId(1));
            schedule.run(&mut world);
            if action.is_none() || world.resource::<MatchState>().active_player_index.0 != 0 {
                break;
            }
        }
    }
    branch_sim::place_snapshot(&mut world, &md, start, &mut entities);
    branch_sim::place_funds(&mut world, &metrics, from - 1);

    // 切除実験: P1 首都から距離 k 以内の P2 部隊を取り除く
    if let Some(k) = remove_within {
        let capital = world
            .query::<(&GridPosition, &Property)>()
            .iter(&world)
            .find(|(_, p)| p.terrain == Terrain::Capital && p.owner_id == Some(PlayerId(1)))
            .map(|(pos, _)| *pos)
            .unwrap();
        let topology = world.resource::<Map>().topology;
        let doomed = world
            .query::<(Entity, &GridPosition, &Faction)>()
            .iter(&world)
            .filter(|(_, pos, f)| {
                f.0 == PlayerId(2) && topology.distance((pos.x, pos.y), (capital.x, capital.y)) <= k
            })
            .map(|(e, _, _)| e)
            .collect::<Vec<_>>();
        for entity in doomed {
            world.despawn(entity);
        }
    }

    // ラウンド from-1 の終わり（P2 手番の終わり）から、P1 の手番開始処理へ進める
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

    let before = world
        .query::<(Entity, &Faction)>()
        .iter(&world)
        .map(|(e, _)| e)
        .collect::<HashSet<_>>();
    let funds_start = world
        .resource::<Players>()
        .0
        .iter()
        .find(|p| p.id == PlayerId(1))
        .unwrap()
        .funds;
    for _ in 0..400 {
        let action = execute_ai_turn(&mut world, PlayerId(1));
        schedule.run(&mut world);
        if action.is_none() {
            break;
        }
        let active = world.resource::<MatchState>().active_player_index.0;
        if active != 0 {
            break;
        }
    }
    // 手番中に生まれた P1 部隊 = 生産（手番中に撃破されることは P1 手番では起きない前提で反撃死は除外されうる）
    let produced = world
        .query::<(Entity, &GridPosition, &Faction, &UnitStats)>()
        .iter(&world)
        .filter(|(e, _, f, _)| f.0 == PlayerId(1) && !before.contains(e))
        .map(|(_, pos, _, s)| serde_json::json!([pos.x, pos.y, s.unit_type.as_str()]))
        .collect::<Vec<_>>();
    println!(
        "PROBE {}",
        serde_json::json!({
            "log": path,
            "from": from,
            "remove_within": remove_within,
            "funds_start": funds_start,
            "produced": produced,
        })
    );
}
