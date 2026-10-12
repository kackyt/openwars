//! 同一seed・同一AI版の対局が、同じプロセス内で繰り返しても同じ行動列になることを検証する。
//!
//! std の `RandomState` はHashMapごとに異なる鍵を使うため、反復順に依存する判断が
//! 残っていると、同じプロセス内の2局目でも行動列が分岐する。プロセスを跨いだ
//! 揺れ（対戦スイープで観測された通常経路の分岐）と同じ原因を、1プロセスで検出する。

use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::ai::engine::execute_ai_turn;
use engine::components::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;

/// 1ターン内で許容するAIステップ数の上限（無限ループ検出用）
const MAX_STEPS_PER_TURN: usize = 400;

/// 指定ターンまでAI同士で対戦し、行動列を返す。
fn play(map_name: &str, seed: u64, p1: AiVersion, p2: AiVersion, turns: u32) -> Vec<String> {
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
            return actions;
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
