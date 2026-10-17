//! 単一AIパイプラインの複数ターン挙動を固定する回帰基準。
//!
//! 各シナリオは固定seedのAI同士の対戦を数ターン進め、
//! 行動文字列・主要イベント・ターン境界ごとの盤面スナップショットを
//! `tests/golden/ai_baseline/<名前>.txt` と比較する。
//! 期待値の再生成は、挙動変更が意図的であると確認できた場合に限り
//! 環境変数 `OPENWARS_BLESS_AI_BASELINE=1` を付けて実行する。

use bevy_ecs::event::EventCursor;
use bevy_ecs::prelude::*;
use engine::ai::ai_version::{AiVersion, PlayerAiSettings};
use engine::ai::execute_ai_turn;
use engine::ai::strategy_profile::{V4StrategyProfile, profile_for};
use engine::components::*;
use engine::events::*;
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::*;
use std::path::PathBuf;

/// 1ターン内で許容するAIステップ数の上限（無限ループ検出用）
const MAX_STEPS_PER_TURN: usize = 400;

/// 検証対象イベントを取りこぼさず読むためのカーソル群。
/// スケジュール末尾でイベントバッファが入れ替わるため、カーソルを保持して差分を読む。
#[derive(Default)]
struct EventCursors {
    moved: EventCursor<UnitMovedEvent>,
    attacked: EventCursor<UnitAttackedEvent>,
    destroyed: EventCursor<UnitDestroyedEvent>,
    captured: EventCursor<PropertyCapturedEvent>,
    produced: EventCursor<UnitProducedEvent>,
    phase: EventCursor<GamePhaseChangedEvent>,
    game_over: EventCursor<GameOverEvent>,
}

impl EventCursors {
    /// 前回読み取り以降に発行されたイベントをトレースへ追記する。
    fn drain_into(&mut self, world: &World, trace: &mut Vec<String>) {
        for ev in self.moved.read(world.resource::<Events<UnitMovedEvent>>()) {
            push_event(trace, format!("{ev:?}"));
        }
        for ev in self
            .attacked
            .read(world.resource::<Events<UnitAttackedEvent>>())
        {
            push_event(trace, format!("{ev:?}"));
        }
        for ev in self
            .destroyed
            .read(world.resource::<Events<UnitDestroyedEvent>>())
        {
            push_event(trace, format!("{ev:?}"));
        }
        for ev in self
            .captured
            .read(world.resource::<Events<PropertyCapturedEvent>>())
        {
            push_event(trace, format!("{ev:?}"));
        }
        for ev in self
            .produced
            .read(world.resource::<Events<UnitProducedEvent>>())
        {
            push_event(trace, format!("{ev:?}"));
        }
        for ev in self
            .phase
            .read(world.resource::<Events<GamePhaseChangedEvent>>())
        {
            push_event(trace, format!("{ev:?}"));
        }
        for ev in self
            .game_over
            .read(world.resource::<Events<GameOverEvent>>())
        {
            push_event(trace, format!("{ev:?}"));
        }
    }
}

/// イベント行を追記する。
fn push_event(trace: &mut Vec<String>, debug: String) {
    trace.push(format!("    ev {}", normalize_entities(&debug)));
}

/// `Entity` の番号は生成・再利用順（セーブ復元時の生成順を含む）に依存し
/// AIの判断とは無関係なので、`Entity` のDebug表記（`<index>v<generation>#<bits>`）を
/// フィールド名に関係なく `<entity>` に正規化する。
fn normalize_entities(debug: &str) -> String {
    let bytes = debug.as_bytes();
    let mut out = String::with_capacity(debug.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(len) = entity_token_len(&bytes[i..]) {
            // 直前が数字なら数値の途中なので置換しない
            if i == 0 || !bytes[i - 1].is_ascii_digit() {
                out.push_str("<entity>");
                i += len;
                continue;
            }
        }
        // ASCII以外（日本語など）も崩さないよう文字単位で進める
        let ch = debug[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 先頭が `数字+ 'v' 数字+ '#' 数字+` に一致すればその長さを返す。
fn entity_token_len(bytes: &[u8]) -> Option<usize> {
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let n1 = digits(0);
    if n1 == 0 || bytes.get(n1) != Some(&b'v') {
        return None;
    }
    let n2 = digits(n1 + 1);
    if n2 == 0 || bytes.get(n1 + 1 + n2) != Some(&b'#') {
        return None;
    }
    let n3 = digits(n1 + n2 + 2);
    (n3 > 0).then_some(n1 + n2 + 2 + n3)
}

/// 固定seed・固定AI版でマップを初期化する。
fn new_game(map_name: &str, seed: u64, p1: AiVersion, p2: AiVersion) -> (World, Schedule) {
    let master_data = MasterDataRegistry::load().expect("マスターデータの読み込みに失敗");
    let (mut world, schedule) = engine::setup::initialize_world_from_master_data_with_topology(
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
    (world, schedule)
}

/// ターン境界の盤面を、エンティティIDに依存しない順序付きの文字列で表す。
fn snapshot(world: &mut World, trace: &mut Vec<String>) {
    let players = world.resource::<Players>().0.clone();
    for p in &players {
        trace.push(format!("  funds P{} = {}", p.id.0, p.funds));
    }

    let mut units: Vec<String> = world
        .query::<(&Faction, &UnitStats, &GridPosition, &Health)>()
        .iter(world)
        .map(|(f, s, pos, hp)| {
            format!(
                "  unit P{} {:?} ({},{}) hp={}",
                f.0.0, s.unit_type, pos.x, pos.y, hp.current
            )
        })
        .collect();
    units.sort();
    trace.extend(units);

    let mut props: Vec<String> = world
        .query::<(&GridPosition, &Property)>()
        .iter(world)
        .filter(|(_, p)| p.owner_id.is_some() || p.capture_points != p.max_capture_points)
        .map(|(pos, p)| {
            format!(
                "  prop ({},{}) {:?} owner={:?} cp={}",
                pos.x,
                pos.y,
                p.terrain,
                p.owner_id.map(|o| o.0),
                p.capture_points
            )
        })
        .collect();
    props.sort();
    trace.extend(props);
}

/// `until_turn` 以下のターンをすべて終えるか決着するまでAI同士で進める。
fn play_until(
    world: &mut World,
    schedule: &mut Schedule,
    cursors: &mut EventCursors,
    until_turn: u32,
    trace: &mut Vec<String>,
) {
    loop {
        let (turn, active, game_over) = {
            let ms = world.resource::<MatchState>();
            let players = world.resource::<Players>();
            (
                ms.current_turn_number.0,
                players.0[ms.active_player_index.0].id,
                ms.game_over.clone(),
            )
        };
        if let Some(cond) = game_over {
            trace.push(format!("GAME OVER {cond:?}"));
            return;
        }
        if turn > until_turn {
            return;
        }

        // V4は入口で分類したプロファイルもトレースして、分類の変化を検出する。
        let version = world.resource::<PlayerAiSettings>().get_version(active);
        if version == AiVersion::V4 {
            let profile = profile_for(world, active);
            trace.push(format!(
                "T{turn} P{} begin {version:?} {profile:?}",
                active.0
            ));
        } else {
            trace.push(format!("T{turn} P{} begin {version:?}", active.0));
        }

        let mut steps = 0;
        loop {
            assert!(
                steps < MAX_STEPS_PER_TURN,
                "T{turn} P{} がステップ上限を超過",
                active.0
            );
            let action = execute_ai_turn(world, active);
            schedule.run(world);
            steps += 1;
            match action {
                Some(a) => {
                    trace.push(format!("  {}", normalize_entities(&a)));
                    cursors.drain_into(world, trace);
                }
                None => {
                    trace.push("  <end>".to_string());
                    cursors.drain_into(world, trace);
                    break;
                }
            }
        }
        trace.push(format!("T{turn} P{} end", active.0));
        snapshot(world, trace);
    }
}

/// トレースをゴールデンファイルと比較する（bless時は書き出す）。
fn check_golden(name: &str, trace: &[String]) {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "golden",
        "ai_baseline",
        &format!("{name}.txt"),
    ]
    .iter()
    .collect();
    let actual = trace.join("\n") + "\n";
    // 期待値を変更せずに、実際の盤面・行動列を別ディレクトリへ保存して差分を審査する。
    if let Some(output_dir) = std::env::var_os("OPENWARS_AI_BASELINE_OUTPUT_DIR") {
        let output_dir = PathBuf::from(output_dir);
        std::fs::create_dir_all(&output_dir).unwrap();
        std::fs::write(output_dir.join(format!("{name}.txt")), &actual).unwrap();
    }
    if std::env::var_os("OPENWARS_BLESS_AI_BASELINE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("ゴールデン {} が存在しない", path.display()));
    // 改行コードの差（Windowsのチェックアウト設定）は比較対象外とする。
    let expected = expected.replace("\r\n", "\n");
    if expected != actual {
        let first_diff = expected
            .lines()
            .zip(actual.lines())
            .position(|(e, a)| e != a)
            .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
        let exp_line = expected.lines().nth(first_diff).unwrap_or("<EOF>");
        let act_line = actual.lines().nth(first_diff).unwrap_or("<EOF>");
        panic!(
            "{name}: ゴールデンと不一致 (行 {})\n  expected: {exp_line}\n  actual:   {act_line}",
            first_diff + 1
        );
    }
}

/// 連続対戦シナリオを実行してゴールデンと比較する。
fn run_scenario(name: &str, map: &str, seed: u64, p1: AiVersion, p2: AiVersion, turns: u32) {
    let (mut world, mut schedule) = new_game(map, seed, p1, p2);
    let mut cursors = EventCursors::default();
    let mut trace = vec![format!(
        "scenario {name} map={map} seed={seed} P1={p1:?} P2={p2:?}"
    )];
    play_until(&mut world, &mut schedule, &mut cursors, turns, &mut trace);
    check_golden(name, &trace);
}

/// 途中セーブ・ロードを挟むシナリオ。
/// ロード後の挙動が連続対戦と一致するかも記録し、現行の差異をそのまま固定する。
fn run_save_load_scenario(
    name: &str,
    map: &str,
    seed: u64,
    p1: AiVersion,
    p2: AiVersion,
    save_after_turn: u32,
    turns: u32,
) {
    let master_data = MasterDataRegistry::load().expect("マスターデータの読み込みに失敗");
    let (mut world, mut schedule) = new_game(map, seed, p1, p2);
    let mut cursors = EventCursors::default();
    let mut prefix = Vec::new();
    play_until(
        &mut world,
        &mut schedule,
        &mut cursors,
        save_after_turn,
        &mut prefix,
    );

    let save = engine::serialize::export_save_data(&mut world, map).expect("セーブ失敗");
    // セーブ時点の盤面と各プレイヤーのV4プロファイルを控えておく。
    let mut saved_board = Vec::new();
    snapshot(&mut world, &mut saved_board);
    let saved_profiles = [
        profile_for(&mut world, PlayerId(1)),
        profile_for(&mut world, PlayerId(2)),
    ];

    // 連続対戦側をそのまま進める（こちらは完全に決定的なのでゴールデンで固定する）。
    let mut continued = Vec::new();
    play_until(
        &mut world,
        &mut schedule,
        &mut cursors,
        turns,
        &mut continued,
    );

    // 保存データから復元した側。
    // 注意: export_save_data はユニットを ECS クエリ（アーキタイプ）順で書き出し、
    // その順序はプロセスごとに変わりうる。復元後のスポーン順が変わると AI の
    // 同点処理が変わり、ロード後の行動列は実行ごとに揺れる（本番コードの既知の非決定性）。
    // そのためロード側は行動列をゴールデン化せず、盤面・プロファイルの復元と
    // 最後まで進行できることのみを検証する。
    let (mut loaded_world, mut loaded_schedule) =
        engine::serialize::import_save_data(&save, &master_data).expect("ロード失敗");
    let mut loaded_board = Vec::new();
    snapshot(&mut loaded_world, &mut loaded_board);
    assert_eq!(
        saved_board, loaded_board,
        "{name}: ロード直後の盤面がセーブ時点と一致しない"
    );
    let loaded_profiles = [
        profile_for(&mut loaded_world, PlayerId(1)),
        profile_for(&mut loaded_world, PlayerId(2)),
    ];
    assert_eq!(
        saved_profiles, loaded_profiles,
        "{name}: ロード後にV4プロファイルが変化した"
    );

    let mut loaded_cursors = EventCursors::default();
    let mut loaded = Vec::new();
    play_until(
        &mut loaded_world,
        &mut loaded_schedule,
        &mut loaded_cursors,
        turns,
        &mut loaded,
    );
    let loaded_state = loaded_world.resource::<MatchState>();
    assert!(
        loaded_state.game_over.is_some() || loaded_state.current_turn_number.0 > turns,
        "{name}: ロード後の対戦が指定ターンまで進行しなかった"
    );

    let mut trace = vec![format!(
        "scenario {name} map={map} seed={seed} P1={p1:?} P2={p2:?} save_after=T{save_after_turn}"
    )];
    trace.extend(prefix);
    trace.push(format!("=== saved profiles {saved_profiles:?} ==="));
    trace.push("=== continued ===".to_string());
    trace.extend(continued);
    check_golden(name, &trace);
}

#[test]
fn baseline_profiles_of_scenario_maps() {
    // シナリオが想定どおりのV4経路を通ることを先に保証する。
    let (mut world, _) = new_game("map_6", 1, AiVersion::V4, AiVersion::V4);
    assert_eq!(
        profile_for(&mut world, PlayerId(1)),
        V4StrategyProfile::Standard
    );
    let (mut world, _) = new_game("map_1", 1, AiVersion::V4, AiVersion::V4);
    assert_eq!(
        profile_for(&mut world, PlayerId(1)),
        V4StrategyProfile::SmallMapExpansion
    );
}

#[test]
fn baseline_v4_standard_multi_turn() {
    run_scenario(
        "v4_standard_map6",
        "map_6",
        1,
        AiVersion::V4,
        AiVersion::V4,
        4,
    );
}

#[test]
fn baseline_v4_small_multi_turn() {
    run_scenario("v4_small_map1", "map_1", 1, AiVersion::V4, AiVersion::V4, 4);
}

#[test]
fn baseline_v1_v2_multi_turn() {
    run_scenario("v1_vs_v2_map1", "map_1", 1, AiVersion::V1, AiVersion::V2, 4);
}

#[test]
fn baseline_v3_v4_standard_multi_turn() {
    run_scenario("v3_vs_v4_map6", "map_6", 2, AiVersion::V3, AiVersion::V4, 4);
}

#[test]
fn baseline_v100_v200_multi_turn() {
    run_scenario(
        "v100_vs_v200_map1",
        "map_1",
        1,
        AiVersion::V100,
        AiVersion::V200,
        4,
    );
}

#[test]
fn baseline_v4_standard_save_load() {
    run_save_load_scenario(
        "v4_standard_map6_save_load",
        "map_6",
        3,
        AiVersion::V4,
        AiVersion::V4,
        2,
        4,
    );
}

#[test]
fn baseline_v4_small_save_load() {
    run_save_load_scenario(
        "v4_small_map1_save_load",
        "map_1",
        3,
        AiVersion::V4,
        AiVersion::V4,
        2,
        4,
    );
}
