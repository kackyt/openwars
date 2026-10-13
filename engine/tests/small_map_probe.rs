//! 小規模マップ拡張の要否を判定するための再現プローブ（一時）。
//!
//! 小規模マップでV4を先後両方で対戦させ、行動列をファイルへ書き出す。

use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::ai::execute_ai_turn;
use engine::components::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;
use std::path::PathBuf;

const MAPS: [&str; 9] = [
    "map_1", "map_2", "map_5", "map_12", "map_13", "map_15", "map_29", "map_32", "map_42",
];

fn run(map: &str, seed: u64, p1: AiVersion, p2: AiVersion, turns: u32) -> Vec<String> {
    let master_data = MasterDataRegistry::load().unwrap();
    let (mut world, mut schedule) = engine::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        map,
        GridTopology::Hex,
    )
    .unwrap();
    world.insert_resource(GameRng::new(seed));
    let mut settings = PlayerAiSettings::default();
    settings.set_version(PlayerId(1), p1);
    settings.set_version(PlayerId(2), p2);
    world.insert_resource(settings);
    let mut trace = vec![];
    loop {
        let (turn, active, over) = {
            let ms = world.resource::<MatchState>();
            let players = world.resource::<Players>();
            (
                ms.current_turn_number.0,
                players.0[ms.active_player_index.0].id,
                ms.game_over.clone(),
            )
        };
        if over.is_some() {
            trace.push(format!("GAME OVER {over:?}"));
            break;
        }
        if turn > turns {
            break;
        }
        trace.push(format!("T{turn} P{}", active.0));
        for _ in 0..400 {
            let action = execute_ai_turn(&mut world, active);
            schedule.run(&mut world);
            match action {
                Some(a) => trace.push(a),
                None => break,
            }
        }
    }
    // 終局時の戦況（拠点数・ユニット数・ユニット価値）を記録する。
    let mut summary = vec![];
    for player in [PlayerId(1), PlayerId(2)] {
        let props = world
            .query::<(&Property, &Faction)>()
            .iter(&world)
            .filter(|(_, f)| f.0 == player)
            .count();
        let mut units = 0;
        let mut value = 0u64;
        for (faction, health, stats) in world
            .query::<(&Faction, &Health, &UnitStats)>()
            .iter(&world)
        {
            if faction.0 == player {
                units += 1;
                value += u64::from(stats.cost) * u64::from(health.current)
                    / u64::from(health.max.max(1));
            }
        }
        summary.push(format!(
            "P{} props={props} units={units} value={value}",
            player.0
        ));
    }
    trace.push(format!("SUMMARY {}", summary.join(" ")));
    trace
}

#[test]
#[ignore]
fn small_map_probe() {
    let out: PathBuf = std::env::var_os("OPENWARS_PROBE_OUT")
        .expect("OPENWARS_PROBE_OUT")
        .into();
    std::fs::create_dir_all(&out).unwrap();
    let turns = std::env::var("OPENWARS_PROBE_TURNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let seed = std::env::var("OPENWARS_PROBE_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    for map in MAPS {
        for (p1, p2) in [
            (AiVersion::V4, AiVersion::V200),
            (AiVersion::V200, AiVersion::V4),
            (AiVersion::V4, AiVersion::V4),
        ] {
            let name = format!("{map}_{p1:?}_{p2:?}");
            let trace = run(map, seed, p1, p2, turns);
            std::fs::write(out.join(format!("{name}.txt")), trace.join("\n")).unwrap();
        }
    }
}
