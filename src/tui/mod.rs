pub mod app;
pub mod event;
pub mod input;
pub mod modal;
pub mod snapshot;
mod tabs;
mod ui;

use std::io;
use std::panic;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tracing::error;

use self::app::{App, TuiCommand};
use self::event::{AppEvent, run_event_loop};
use self::snapshot::DashboardSnapshot;

/// Run the TUI dashboard.
///
/// - `snapshot_rx`: receives DashboardSnapshot from orchestrator
/// - `cmd_tx`: sends TuiCommand back to orchestrator (Quit, RecoverL3)
///
/// This function owns the terminal. On exit (normal or panic), it restores
/// the terminal to its original state.
pub async fn run_tui(
    snapshot_rx: mpsc::Receiver<DashboardSnapshot>,
    cmd_tx: mpsc::Sender<TuiCommand>,
) -> io::Result<()> {
    // Set up panic hook to restore terminal on panic
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = restore_terminal();
        original_hook(panic_info);
    }));

    // Initialize terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Create app state
    let mut app = App::new(cmd_tx);

    // Event channel: merge keyboard + tick + snapshots
    let (event_tx, mut event_rx) = mpsc::channel::<AppEvent>(64);

    // Spawn event loop in background
    tokio::spawn(run_event_loop(event_tx, snapshot_rx));

    // Main render loop
    loop {
        // Render current state
        terminal.draw(|frame| ui::render(&app, frame))?;

        // Wait for next event
        let Some(event) = event_rx.recv().await else {
            break;
        };

        match event {
            AppEvent::Key(key) => {
                app.handle_key(key);
                if app.should_quit {
                    break;
                }
            }
            AppEvent::Tick => {
                // Just triggers a redraw (handled by the draw call above)
            }
            AppEvent::Snapshot(snap) => {
                // If the snapshot carries search results, push them to the
                // active SearchMarket modal so the user sees them immediately.
                if let Some(ref results) = snap.search_results {
                    app.update_search_results(results.clone());
                }
                app.snapshot = Some(snap);
            }
        }
    }

    // Cleanup
    if let Err(e) = restore_terminal() {
        error!("Failed to restore terminal: {e}");
    }

    Ok(())
}

fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    Ok(())
}
