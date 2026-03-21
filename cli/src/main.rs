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
use std::{error::Error, io};

fn main() -> Result<(), Box<dyn Error>> {
    // 1. Setup App state
    let mut app = App::new();

    // 2. Terminal setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // 3. Main Event/Draw loop
    let res = run_app(&mut terminal, &mut app);

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

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui::ui(f, app))?;

        if event::poll(std::time::Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
                && key.kind == event::KeyEventKind::Press {
                    app.handle_key(key);
                }

        if app.should_quit {
            return Ok(());
        }

        // Run systems if in-game
        if app.ui_state.current_screen == app::CurrentScreen::InGame
            && let (Some(world), Some(schedule)) = (&mut app.world, &mut app.schedule) {
                schedule.run(world);
            }
    }
}
