//! 実対局の途中盤面を同条件で復元し、局地攻略の候補を実エンジンで比較する診断。
//! 通常テストでは実行せず、scripts/run_assault_probe.py から局面を明示して起動する。
#[path = "branch_sim.rs"]
mod branch_fixture;

use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::components::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;
use std::collections::HashMap;

#[test]
#[ignore]
fn inspect_assault_scenario() -> anyhow::Result<()> {
    let input = std::env::var("ASSAULT_LOG")?;
    let map_name = std::env::var("ASSAULT_MAP")?;
    let turn: u32 = std::env::var("ASSAULT_FROM")?.parse()?;
    let player = PlayerId(std::env::var("ASSAULT_PLAYER")?.parse()?);
    let horizon: u32 = std::env::var("ASSAULT_TURNS")?.parse()?;
    let output = std::env::var("ASSAULT_OUT")?;
    anyhow::ensure!(turn > 1, "ラウンド末盤面から復元するため開始手番は2以上");
    let data: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&input)?)?;
    let game = &data["results"][0];
    let snapshot = game["strategic_history"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("strategic_historyがない"))?
        .iter()
        .find(|row| row["turn"].as_u64() == Some(u64::from(turn - 1)))
        .ok_or_else(|| anyhow::anyhow!("出発点のラウンド末盤面がない"))?;
    let metrics = game["metrics"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("metricsがない"))?;
    let master = MasterDataRegistry::load()?;
    let (mut world, mut schedule) = engine::setup::initialize_world_from_master_data_with_topology(
        &master,
        &map_name,
        GridTopology::Hex,
    )?;
    let seed = game["seed"].as_u64().unwrap_or(1);
    world.insert_resource(GameRng::new(seed));
    let mut settings = PlayerAiSettings::default();
    for side in [PlayerId(1), PlayerId(2)] {
        settings.set_version(side, if side == player { AiVersion::V4 } else { AiVersion::V200 });
    }
    world.insert_resource(settings);
    let mut entities = HashMap::new();
    branch_fixture::place_snapshot(&mut world, &master, snapshot, &mut entities);
    branch_fixture::place_funds(&mut world, metrics, turn - 1);
    // 盤面を各案へ保存・復元しても、同じログ部隊を同じUUIDで追えるようにする。
    for (&id, &entity) in &entities {
        world.entity_mut(entity).insert(UnitId(uuid::Uuid::from_u128(u128::from(id))));
    }
    // 配置・HP・所有者・積荷の組を元ログと照合してから手番を開始する。
    for row in snapshot["units"].as_array().unwrap() {
        let id = row["unit_id"].as_u64().unwrap();
        let entity = entities[&id];
        assert_eq!(world.get::<Health>(entity).unwrap().current, row["hp"].as_u64().unwrap() as u32);
        if let Some(carrier) = row["transporting_by"].as_u64() {
            assert_eq!(world.get::<Transporting>(entity).unwrap().0, entities[&carrier]);
        } else {
            assert_eq!(*world.get::<GridPosition>(entity).unwrap(), GridPosition {
                x: row["x"].as_u64().unwrap() as usize,
                y: row["y"].as_u64().unwrap() as usize,
            });
        }
    }
    {
        let last = world.resource::<Players>().0.len() - 1;
        let mut state = world.resource_mut::<MatchState>();
        state.current_turn_number = TurnNumber(turn - 1);
        state.active_player_index = PlayerIndex(last);
        state.current_phase = Phase::EndTurn;
        state.game_over = None;
    }
    engine::systems::turn_management::advance_next_phase(&mut world);
    schedule.run(&mut world);
    let mut preparatory_commands = 0;
    // 後手の比較は、共通のP1手番を1回進めた後の同一P2開始盤面から行う。
    loop {
        let state = world.resource::<MatchState>();
        anyhow::ensure!(state.game_over.is_none(), "対象の手番より前に終局した");
        let active = world.resource::<Players>().0[state.active_player_index.0].id;
        if active == player && state.current_phase == Phase::Main { break; }
        anyhow::ensure!(preparatory_commands < 1000, "対象の手番へ到達しない");
        engine::ai::execute_ai_turn(&mut world, active);
        schedule.run(&mut world);
        preparatory_commands += 1;
    }
    let objective = world.iter_entities().find_map(|entity| {
        let property = entity.get::<Property>()?;
        (property.terrain == Terrain::Capital && property.owner_id.is_some_and(|owner| owner != player))
            .then(|| entity.get::<GridPosition>().copied()).flatten()
    }).ok_or_else(|| anyhow::anyhow!("敵首都がない"))?;
    use engine::ai::assault_probe::ProbePlan;
    // 案を絞る場合は ASSAULT_PLANS=baseline,advance のように指定する。
    let plans = match std::env::var("ASSAULT_PLANS") {
        Ok(text) => text.split(',').map(|name| match name.trim() {
            "baseline" => Ok(ProbePlan::Baseline),
            "counter_battery" => Ok(ProbePlan::CounterBattery),
            "advance" => Ok(ProbePlan::Advance),
            other => Err(anyhow::anyhow!("未知の案: {other}")),
        }).collect::<anyhow::Result<Vec<_>>>()?,
        Err(_) => vec![ProbePlan::Baseline, ProbePlan::CounterBattery, ProbePlan::Advance],
    };
    let report = engine::ai::assault_probe::compare_plans(
        &mut world, &master, &map_name, player, objective, horizon, &plans,
    )?;
    let result = serde_json::json!({
        "scenario": {"input":input,"map":map_name,"seed":seed,"turn":turn,"player":player.0,
            "source_snapshot_round":turn-1,"preparatory_commands":preparatory_commands,"own_turns":horizon,
            "limitations":["ログに残弾・燃料がないため満タン復元","乱数は局面の内部状態ではなく元seedで初期化",
                "AI記憶は元対局と一致しない","後手開始は共通の先手再実行から作成"]},
        "report": report,
    });
    std::fs::write(output, serde_json::to_vec_pretty(&result)?)?;
    Ok(())
}
