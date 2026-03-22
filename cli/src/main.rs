mod app;
mod ui;

use app::App;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
};
#[cfg(feature = "ai-debug")]
use ratatui::backend::TestBackend;
#[cfg(feature = "ai-debug")]
use std::io::{self, BufRead};
#[cfg(not(feature = "ai-debug"))]
use std::{error::Error, io};
#[cfg(feature = "ai-debug")]
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "ai-debug")]
    {
        return run_ai_debug();
    }

    // 1. Setup App state
    let mut app = App::new();

    // 2. Terminal setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // 3. Main Event/Draw loop
    let get_event = |_timeout: std::time::Duration| -> io::Result<Option<Event>> {
        if event::poll(std::time::Duration::from_millis(50))? {
            Ok(Some(event::read()?))
        } else {
            Ok(None)
        }
    };
    let on_draw = |_terminal: &Terminal<CrosstermBackend<std::io::Stdout>>| {};

    let res = run_app(&mut terminal, &mut app, get_event, on_draw);

    // 4. Terminal Teardown
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

#[cfg(feature = "ai-debug")]
fn run_ai_debug() -> Result<(), Box<dyn Error>> {
    use ratatui::backend::TestBackend;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyEventState};
    use std::io::{self, BufRead};
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut app = App::new();
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend)?;

    println!("--- AI TUI Debugger Started ---");
    println!("Commands: 'up', 'down', 'left', 'right', 'enter', 'esc', 'space', 'dump', 'q' (quit), or single chars like 'j' or 'T'.");

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    static SHOULD_DUMP: AtomicBool = AtomicBool::new(false);

    let get_event = |_timeout: std::time::Duration| -> io::Result<Option<Event>> {
        // AI Wait for input
        let line = lines.next();
        if let Some(Ok(line_str)) = line {
            let cmd = line_str.trim();
            if cmd == "q" || cmd == "quit" {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Quit requested by AI"));
            }
            if cmd == "dump" {
                SHOULD_DUMP.store(true, Ordering::SeqCst);
                // Send dummy event to force render loop
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

        if let Some(Event::Key(key)) = get_event(std::time::Duration::from_millis(50))? {
            if key.kind == event::KeyEventKind::Press {
                app.handle_key(key);
            }
        }

        if app.should_quit {
            return Ok(());
        }

        // Run systems if in-game
        if app.ui_state.current_screen == app::CurrentScreen::InGame {
            let mut popup_msg = None;
            if let (Some(world), Some(schedule)) = (&mut app.world, &mut app.schedule) {
                schedule.run(world);

                use bevy_ecs::event::Events;
                use openwars_engine::events::{GamePhaseChangedEvent, UnitAttackedEvent};

                if let Some(mut events) = world.get_resource_mut::<Events<UnitAttackedEvent>>() {
                    for ev in events.drain() {
                        let text = if let Some(cdmg) = ev.counter_damage_dealt {
                            format!(
                                "Combat Result\nDamage dealt: {}0%\nCounter damage: {}0%",
                                ev.damage_dealt, cdmg
                            )
                        } else {
                            format!("Combat Result\nDamage dealt: {}0%", ev.damage_dealt)
                        };
                        popup_msg = Some(text);
                    }
                }

                if popup_msg.is_none()
                    && let Some(mut phase_events) =
                        world.get_resource_mut::<Events<GamePhaseChangedEvent>>()
                {
                    for ev in phase_events.drain() {
                        popup_msg = Some(format!(
                            "Player {}'s Turn\n\nPress Space to continue...",
                            ev.active_player.0
                        ));
                    }
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
