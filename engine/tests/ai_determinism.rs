//! 同一seed・同一AI版の対局が、繰り返しても同じ行動列になることを検証する。
//!
//! 同じプロセス内で同じ対局を2回行い、行動列を比べる。プロセスを跨いだ再現性
//! （ahashの鍵がプロセスごとに変わり、ECSクエリの列挙順が変わる問題）は1プロセスでは
//! 検出できないため、`print_decision_fingerprints` の出力を別プロセス間で比べる。

use bevy_ecs::prelude::*;
use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::ai::engine::execute_ai_turn;
use engine::components::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;

/// 1ターン内で許容するAIステップ数の上限（無限ループ検出用）
const MAX_STEPS_PER_TURN: usize = 400;

/// 指定ターンまでAI同士で対戦し、行動列を返す。
fn play(map_name: &str, seed: u64, p1: AiVersion, p2: AiVersion, turns: u32) -> Vec<String> {
    play_with_outcome(map_name, seed, p1, p2, turns).0
}

/// 対戦し、行動列と終局状態（勝者・ターン・各playerの物件数とunit価値）を返す。
fn play_with_outcome(
    map_name: &str,
    seed: u64,
    p1: AiVersion,
    p2: AiVersion,
    turns: u32,
) -> (Vec<String>, String) {
    let master_data = MasterDataRegistry::load().expect("マスターデータの読み込みに失敗");
    let (mut world, mut schedule) = engine::setup::initialize_world_from_master_data_with_topology(
        &master_data,
        map_name,
        GridTopology::Hex,
    )
    .expect("ワールド初期化に失敗");
    world.insert_resource(GameRng::new(seed));
    let mut ai_settings = PlayerAiSettings::default();
    ai_settings.set_version(PlayerId(1), p1);
    ai_settings.set_version(PlayerId(2), p2);
    world.insert_resource(ai_settings);

    let mut actions = Vec::new();
    loop {
        let (turn, active, game_over) = {
            let ms = world.resource::<MatchState>();
            let players = world.resource::<Players>();
            (
                ms.current_turn_number.0,
                players.0[ms.active_player_index.0].id,
                ms.game_over.is_some(),
            )
        };
        if game_over || turn > turns {
            let over = format!("{:?}", world.resource::<MatchState>().game_over);
            let summary = outcome_summary(&mut world);
            let outcome = format!("turn={} over={over} {summary}", turn.min(turns));
            return (actions, outcome);
        }
        for step in 0.. {
            assert!(step < MAX_STEPS_PER_TURN, "T{turn} がステップ上限を超過");
            let action = execute_ai_turn(&mut world, active);
            schedule.run(&mut world);
            match action {
                Some(action) => actions.push(format!("T{turn} P{} {action}", active.0)),
                None => break,
            }
        }
    }
}

/// 各playerの所有物件数とunit価値（生産費×HP比）の要約。
fn outcome_summary(world: &mut World) -> String {
    let mut parts = Vec::new();
    for player in [PlayerId(1), PlayerId(2)] {
        let properties = world
            .query::<&Property>()
            .iter(world)
            .filter(|property| property.owner_id == Some(player))
            .count();
        let value: u32 = world
            .query::<(&Faction, &UnitStats, &Health)>()
            .iter(world)
            .filter(|(faction, _, _)| faction.0 == player)
            .map(|(_, stats, health)| stats.cost * health.current / health.max.max(1))
            .sum();
        parts.push(format!("P{}:props={properties},value={value}", player.0));
    }
    parts.join(" ")
}

/// 2局の行動列を比べ、最初に分岐した手を示して失敗させる。
fn assert_replays_identically(map_name: &str, seed: u64, p1: AiVersion, p2: AiVersion, turns: u32) {
    let first = play(map_name, seed, p1, p2, turns);
    let second = play(map_name, seed, p1, p2, turns);
    if first != second {
        let index = first
            .iter()
            .zip(&second)
            .position(|(a, b)| a != b)
            .unwrap_or(first.len().min(second.len()));
        panic!(
            "{map_name} seed={seed}: 同一条件の2局目が {index} 手目で分岐\n  1局目: {}\n  2局目: {}",
            first.get(index).map_or("<end>", String::as_str),
            second.get(index).map_or("<end>", String::as_str),
        );
    }
}

#[test]
fn standard_map_v4_first_replays_identically() {
    assert_replays_identically("map_6", 1, AiVersion::V4, AiVersion::V200, 30);
}

#[test]
fn standard_map_v4_second_replays_identically() {
    assert_replays_identically("map_6", 1, AiVersion::V200, AiVersion::V4, 30);
}

#[test]
fn standard_map_25_replays_identically() {
    assert_replays_identically("map_25", 2, AiVersion::V4, AiVersion::V200, 30);
}

#[test]
fn small_map_replays_identically() {
    assert_replays_identically("map_1", 1, AiVersion::V4, AiVersion::V200, 8);
}

/// 対戦スイープと同じ条件の行動列指紋を出力する（`OPENWARS_FINGERPRINT=1` のときだけ実行）。
///
/// 旧新の実装で同じ指紋になれば、その条件で判断・順序・出力が一致している。
#[test]
fn print_decision_fingerprints() {
    if std::env::var_os("OPENWARS_FINGERPRINT").is_none() {
        return;
    }
    use std::hash::{Hash, Hasher};
    let maps = std::env::var("OPENWARS_FINGERPRINT_MAPS")
        .unwrap_or_else(|_| "map_1,map_2,map_6,map_25".to_string());
    for map in maps.split(',') {
        let seeds: Vec<u64> = std::env::var("OPENWARS_FINGERPRINT_SEEDS")
            .map(|v| v.split(',').filter_map(|s| s.parse().ok()).collect())
            .unwrap_or_else(|_| vec![1, 2]);
        let orders = match std::env::var("OPENWARS_FINGERPRINT_ORDER").as_deref() {
            Ok("swapped") => vec![(AiVersion::V200, AiVersion::V4)],
            Ok("as-given") => vec![(AiVersion::V4, AiVersion::V200)],
            _ => vec![
                (AiVersion::V4, AiVersion::V200),
                (AiVersion::V200, AiVersion::V4),
            ],
        };
        let turns = std::env::var("OPENWARS_FINGERPRINT_TURNS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        for seed in seeds.iter().copied() {
            for (p1, p2) in orders.iter().copied() {
                let (actions, outcome) = play_with_outcome(map, seed, p1, p2, turns);
                if let Some(dir) = std::env::var_os("OPENWARS_FINGERPRINT_DUMP") {
                    let path = std::path::PathBuf::from(dir).join(format!(
                        "{map}_s{seed}_{p1:?}-{p2:?}_{}.txt",
                        std::process::id()
                    ));
                    std::fs::write(path, actions.join("\n")).unwrap();
                }
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                actions.hash(&mut hasher);
                println!(
                    "FINGERPRINT {map} seed={seed} {p1:?}-{p2:?} actions={} hash={:016x} {outcome}",
                    actions.len(),
                    hasher.finish()
                );
            }
        }
    }
}
