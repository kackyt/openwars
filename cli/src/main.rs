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

    println!("--- AI TUI Debugger Started ---");
    println!(
        "Commands: 'up', 'down', 'left', 'right', 'enter', 'esc', 'space', 'dump', 'q' (quit), or single chars like 'j' or 'T'."
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
                    "Quit requested by AI",
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
                schedule.run(world);

                use bevy_ecs::event::Events;
                use engine::events::{GamePhaseChangedEvent, UnitAttackedEvent};

                if let Some(mut events) = world.get_resource_mut::<Events<UnitAttackedEvent>>() {
                    for ev in events.drain() {
                        let a_before_disp = (ev.attacker_hp_before.saturating_add(9)) / 10;
                        let a_after_disp = (ev.attacker_hp_after.saturating_add(9)) / 10;
                        let d_before_disp = (ev.defender_hp_before.saturating_add(9)) / 10;
                        let d_after_disp = (ev.defender_hp_after.saturating_add(9)) / 10;

                        let text = format!(
                            "Combat Result\nAttacker HP: {} -> {}\nDefender HP: {} -> {}",
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
                                "Player {}'s Turn\n\nPress Space to continue...",
                                ev.active_player.0
                            ));
                        }
                    }
                }

                if popup_msg.is_none() {
                    popup_msg = phase_popup;
                }

                // メモリリーク対策: Bevy 0.15.2 はアップデートシステムがないと自動でイベントをクリアしない
                use engine::events::*;
                if let Some(mut e) = world.get_resource_mut::<Events<ProduceUnitCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<AttackUnitCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<CapturePropertyCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<MergeUnitCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<SupplyUnitCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<LoadUnitCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<UnloadUnitCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<WaitUnitCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<UndoMoveCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<NextPhaseCommand>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<UnitMovedEvent>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<UnitDestroyedEvent>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<UnitMergedEvent>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<PropertyCapturedEvent>>() {
                    e.clear();
                }
                if let Some(mut e) = world.get_resource_mut::<Events<GameOverEvent>>() {
                    e.clear();
                }
            }

            if let Some(msg) = popup_msg {
                let current_state = app.ui_state.in_game_state.clone();
                if !matches!(current_state, app::InGameState::EventPopup { .. }) {
                    app.ui_state.in_game_state = app::InGameState::EventPopup { message: msg };
                }
            }
        }
    }
}
