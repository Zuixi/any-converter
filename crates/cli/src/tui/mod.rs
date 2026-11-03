//! `stats --tui`: the live usage screen.
//!
//! One blocking loop: draw, wait at most [`TICK`] for a key, re-query when a
//! dimension changed or the data is [`REFRESH_EVERY`] old. The rollup answers in
//! single-digit milliseconds for a single user, so a channel/select mesh would
//! only add ways to lose a key press or a frame.

pub mod app;
pub mod theme;
pub mod view;

/// The one-line timeline the screen draws: a pure function of the summary.
mod timeline;

use std::io;
use std::panic;
use std::time::Duration;

use any_converter_server::storage::{Granularity, SplitDim, SqliteStorage};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub use app::{App, Filters, Flow, Metric, Preset};

/// Longest a draw waits for a key before the frame is redrawn.
const TICK: Duration = Duration::from_millis(250);
/// Age at which the numbers are pulled again, paused aside.
const REFRESH_EVERY: Duration = Duration::from_secs(1);

/// The dimensions the screen opens on, taken from the command line.
#[derive(Debug, Clone)]
pub struct TuiConfig {
    pub preset: Preset,
    pub granularity: Granularity,
    pub split: Option<SplitDim>,
    pub metric: Metric,
    pub filters: Filters,
}

/// Runs the screen until `q`, `Esc`, or a terminal error.
pub fn run(cfg: TuiConfig, store: SqliteStorage) -> io::Result<()> {
    let mut app =
        App::new(cfg.preset, cfg.granularity, cfg.split, cfg.metric).with_filters(cfg.filters);
    // Before the first draw, so the opening frame is never a frame of zeros.
    app.refresh(&store);

    let screen = Screen::enter()?;
    // Chain onto the hook that is installed now (the default one, which prints
    // the panic): without this a panic would unwind into the alternate screen
    // and leave the terminal unusable, hiding its own message.
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        Screen::restore();
        previous(info);
    }));

    let result = event_loop(&mut app, &store);
    // Every exit path restores the terminal, including the error one.
    drop(screen);
    result
}

/// The alternate screen plus raw mode, undone by `Drop` and by the panic hook.
/// Restoring twice is harmless, which is what makes both paths safe.
struct Screen;

impl Screen {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }

    fn restore() {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        Self::restore();
    }
}

fn event_loop(app: &mut App, store: &SqliteStorage) -> io::Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;

    loop {
        terminal.draw(|frame| view::render(frame, app))?;

        if event::poll(TICK)? {
            // A key release is not a key press; without this check every key
            // would be handled twice on terminals that report both.
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && app.on_key(key) == Flow::Quit {
                    return Ok(());
                }
            }
        }

        if !app.paused && (app.dirty || app.last_refresh.elapsed() >= REFRESH_EVERY) {
            app.refresh(store);
        }
    }
}
