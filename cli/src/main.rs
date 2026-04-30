mod app;
mod ui;

use app::App;
use crossterm::event::Event;
#[cfg(not(feature = "ai-debug"))]
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
#[cfg(not(feature = "ai-debug"))]
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, backend::Backend};
use std::{error::Error, io};

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "ai-debug")]
    {
        run_ai_debug()?;
        Ok(())
    }

    #[cfg(not(feature = "ai-debug"))]
    {
        // 1. アプリケーション状態の初期化
        let mut app = App::new()?;

        // 2. ターミナルのセットアップ
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // 3. メインイベント/描画ループ
        let get_event = |_timeout: std::time::Duration| -> io::Result<Option<Event>> {
            if event::poll(std::time::Duration::from_millis(50))? {
                Ok(Some(event::read()?))
            } else {
                Ok(None)
            }
        };
        let on_draw = |_terminal: &Terminal<CrosstermBackend<std::io::Stdout>>| {};

        let res = run_app(&mut terminal, &mut app, get_event, on_draw);

        // 4. ターミナルの終了処理
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        res.map_err(Into::into)
    }
}

#[cfg(feature = "ai-debug")]
fn run_ai_debug() -> Result<(), Box<dyn Error>> {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;
    use std::io::{self, BufRead};
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut app = App::new()?;
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend)?;

    println!("--- AI TUI デバッガーが開始されました ---");
    println!(
        "コマンド: 'up', 'down', 'left', 'right', 'enter', 'esc', 'space', 'dump', 'q' (終了), または 'j' や 'T' などの単一文字。"
    );

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    static SHOULD_DUMP: AtomicBool = AtomicBool::new(false);

    let get_event = |_timeout: std::time::Duration| -> io::Result<Option<Event>> {
        // AIによる入力待ち
        let line = lines.next();
        if let Some(Ok(line_str)) = line {
            let cmd = line_str.trim();
            if cmd == "q" || cmd == "quit" {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "AIによって終了が要求されました",
                ));
            }
            if cmd == "dump" {
                SHOULD_DUMP.store(true, Ordering::SeqCst);
                // 強制的な描画ループを発生させるダミーイベントを送信
                return Ok(Some(Event::Key(KeyEvent {
                    code: KeyCode::Null,
                    modifiers: KeyModifiers::empty(),
                    kind: KeyEventKind::Press,
                    state: KeyEventState::empty(),
                })));
            }
            let key_code = match cmd {
                "up" => Some(KeyCode::Up),
                "down" => Some(KeyCode::Down),
                "left" => Some(KeyCode::Left),
                "right" => Some(KeyCode::Right),
                "enter" => Some(KeyCode::Enter),
                "esc" => Some(KeyCode::Esc),
                "space" => Some(KeyCode::Char(' ')),
                "" => None,
                s if s.chars().count() == 1 => Some(KeyCode::Char(s.chars().next().unwrap())),
                _ => {
                    println!("Unknown command: {}", cmd);
                    None
                }
            };
            if let Some(code) = key_code {
                return Ok(Some(Event::Key(KeyEvent {
                    code,
                    modifiers: KeyModifiers::empty(),
                    kind: KeyEventKind::Press,
                    state: KeyEventState::empty(),
                })));
            }
        } else {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF"));
        }
        Ok(None)
    };

    let on_draw = |term: &Terminal<TestBackend>| {
        if SHOULD_DUMP.swap(false, Ordering::SeqCst) {
            let buffer = term.backend().buffer();
            println!("=== SCREEN BUFFER DUMP ===");
            for y in 0..buffer.area.height {
                let mut line = String::with_capacity(buffer.area.width as usize);
                for x in 0..buffer.area.width {
                    line.push_str(buffer.get(x, y).symbol());
                }
                println!("{}", line.trim_end());
            }
            println!("==========================");
        }
    };

    if let Err(err) = run_app(&mut terminal, &mut app, get_event, on_draw) {
        println!("Error: {:?}", err);
    }
    Ok(())
}

fn run_app<B: Backend, E, D>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut get_event: E,
    mut on_draw: D,
) -> io::Result<()>
where
    E: FnMut(std::time::Duration) -> io::Result<Option<Event>>,
    D: FnMut(&Terminal<B>),
{
    loop {
        terminal.draw(|f| ui::ui(f, app))?;
        on_draw(terminal);

        if let Some(Event::Key(key)) = get_event(std::time::Duration::from_millis(50))?
            && key.kind == crossterm::event::KeyEventKind::Press
        {
            app.handle_key(key);
        }

        if app.should_quit {
            return Ok(());
        }

        // インゲーム中であればシステムを実行
        if app.ui_state.current_screen == app::CurrentScreen::InGame {
            let mut popup_msg = None;
            if let (Some(world), Some(schedule)) = (&mut app.world, &mut app.schedule) {
                use app::{InGameState, PlayerControlType};
                use bevy_ecs::event::Events;
                use engine::ai::engine::execute_ai_turn;
                use engine::events::{GameOverEvent, GamePhaseChangedEvent, UnitAttackedEvent};
                use engine::resources::{GameOverCondition, MatchState, Players};

                // AIターンの自動進行
                if let InGameState::Normal = app.ui_state.in_game_state {
                    let mut active_player_opt = None;
                    if let Some(match_state) = world.get_resource::<MatchState>()
                        && let Some(players) = world.get_resource::<Players>()
                    {
                        let idx = match_state.active_player_index.0;
                        if let Some(player) = players.0.get(idx) {
                            active_player_opt = Some(player.id);
                        }
                    }

                    if let Some(active_player) = active_player_opt
                        && app.ui_state.player_controls.get(&active_player.0)
                            == Some(&PlayerControlType::Ai)
                        && execute_ai_turn(world, active_player)
                    {
                        app.ui_state.in_game_state = InGameState::WaitAiAction;
                    }
                }

                schedule.run(world);

                if let Some(mut events) = world.get_resource_mut::<Events<UnitAttackedEvent>>() {
                    for ev in events.drain() {
                        let a_before_disp = (ev.attacker_hp_before.saturating_add(9)) / 10;
                        let a_after_disp = (ev.attacker_hp_after.saturating_add(9)) / 10;
                        let d_before_disp = (ev.defender_hp_before.saturating_add(9)) / 10;
                        let d_after_disp = (ev.defender_hp_after.saturating_add(9)) / 10;

                        let text = format!(
                            "戦闘結果\n攻撃側 HP: {} -> {}\n防御側 HP: {} -> {}",
                            a_before_disp, a_after_disp, d_before_disp, d_after_disp
                        );
                        popup_msg = Some(text);
                    }
                }

                let mut phase_popup = None;
                if let Some(mut phase_events) =
                    world.get_resource_mut::<Events<GamePhaseChangedEvent>>()
                {
                    for ev in phase_events.drain() {
                        if ev.new_phase == engine::resources::Phase::Main {
                            phase_popup = Some(format!(
                                "プレイヤー {} のターン\n\nSpaceキーで開始...",
                                ev.active_player.0
                            ));
                        }
                    }
                }

                if let InGameState::WaitAiAction = app.ui_state.in_game_state {
                    // 一定の遅延やイベント処理を待った後にNormalに戻す
                    // （現在のアニメーションやイベント処理機構がないため即座に戻す）
                    app.ui_state.in_game_state = InGameState::Normal;
                }

                if let Some(events) = world.get_resource::<Events<GameOverEvent>>() {
                    let mut cursor = events.get_cursor();
                    if let Some(ev) = cursor.read(events).next() {
                        let condition = ev.condition.clone();
                        let msg = match &condition {
                            GameOverCondition::Winner(pid) => {
                                let players = world.resource::<Players>();
                                if let Some(p) = players.0.iter().find(|p| p.id == *pid) {
                                    format!("勝利：{} 勢力", p.name)
                                } else {
                                    format!("勝利：プレイヤー {:?}", pid)
                                }
                            }
                            GameOverCondition::Draw => "引き分け".to_string(),
                        };
                        app.ui_state.in_game_state = InGameState::GameOverPopup {
                            message: msg,
                            condition,
                        };
                    }
                }

                if popup_msg.is_none() {
                    popup_msg = phase_popup;
                }

                // メモリリーク対策: Bevy 0.15.2 はアップデートシステムがないと自動でイベントをクリアしない
                use engine::events::*;
                macro_rules! clear_events {
                    ($($t:ty),*) => {
                        $(
                            if let Some(mut e) = world.get_resource_mut::<Events<$t>>() {
                                e.clear();
                            }
                        )*
                    };
                }

                clear_events!(
                    ProduceUnitCommand,
                    MoveUnitCommand,
                    AttackUnitCommand,
                    CapturePropertyCommand,
                    MergeUnitCommand,
                    SupplyUnitCommand,
                    LoadUnitCommand,
                    UnloadUnitCommand,
                    WaitUnitCommand,
                    UndoMoveCommand,
                    NextPhaseCommand,
                    GameOverEvent,
                    PropertyCapturedEvent,
                    UnitDestroyedEvent,
                    GamePhaseChangedEvent,
                    UnitMovedEvent,
                    UnitMergedEvent
                );
            }

            if let Some(msg) = popup_msg
                && !matches!(
                    app.ui_state.in_game_state,
                    app::InGameState::EventPopup { .. } | app::InGameState::GameOverPopup { .. }
                )
            {
                app.ui_state.in_game_state = app::InGameState::EventPopup { message: msg };
            }

            if let app::InGameState::WaitActionMenu { unit_entity } = &app.ui_state.in_game_state {
                app.reopen_unit_action_menu(*unit_entity);
            }
        }
    }
}
