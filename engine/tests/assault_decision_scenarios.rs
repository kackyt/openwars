//! 対局ログの各ラウンド終了盤面を復元し、全体前進の GO／WAIT 判定を時系列で出力する。
//! 通常テストでは実行しない。ASSAULT_LOG・ASSAULT_MAP・ASSAULT_PLAYER・ASSAULT_OUT を指定して起動する。
#[path = "branch_sim.rs"]
mod branch_fixture;
use engine::ai::assault_decision::{AssaultDecision, judge_assault};
use engine::components::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;
use std::collections::HashMap;

#[test]
#[ignore]
fn judge_all_rounds() -> anyhow::Result<()> {
    let input = std::env::var("ASSAULT_LOG")?;
    let map_name = std::env::var("ASSAULT_MAP")?;
    let player = PlayerId(std::env::var("ASSAULT_PLAYER")?.parse()?);
    let output = std::env::var("ASSAULT_OUT")?;
    let data: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&input)?)?;
    let game = &data["results"][0];
    let master = MasterDataRegistry::load()?;
    let mut rows = Vec::new();
    // 前回の判定を次のラウンドへ引き継ぐ。
    let mut previous = AssaultDecision::Wait;
    for snapshot in game["strategic_history"].as_array().unwrap() {
        let round = snapshot["turn"].as_u64().unwrap();
        let (mut world, _schedule) =
            engine::setup::initialize_world_from_master_data_with_topology(
                &master,
                &map_name,
                GridTopology::Hex,
            )?;
        let mut entities = HashMap::new();
        branch_fixture::place_snapshot(&mut world, &master, snapshot, &mut entities);
        for (&id, &entity) in &entities {
            world
                .entity_mut(entity)
                .insert(UnitId(uuid::Uuid::from_u128(u128::from(id))));
        }
        let Some(objective) = world.iter_entities().find_map(|e| {
            let p = e.get::<Property>()?;
            (p.terrain == Terrain::Capital && p.owner_id.is_some_and(|o| o != player))
                .then(|| e.get::<GridPosition>().copied())
                .flatten()
        }) else {
            continue;
        };
        let before = entities
            .values()
            .map(|&e| *world.get::<GridPosition>(e).unwrap())
            .collect::<Vec<_>>();
        let report = judge_assault(&mut world, player, objective, previous);
        previous = report.decision;
        // 判定は盤面を動かさない。
        let after = entities
            .values()
            .map(|&e| *world.get::<GridPosition>(e).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(before, after);
        rows.push(serde_json::json!({"round": round, "report": report}));
    }
    std::fs::write(output, serde_json::to_vec_pretty(&rows)?)?;
    Ok(())
}
