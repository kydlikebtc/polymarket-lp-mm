use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyEvent};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::time::interval;

use super::snapshot::DashboardSnapshot;

/// Events consumed by the TUI main loop
pub enum AppEvent {
    /// Keyboard input
    Key(KeyEvent),
    /// Render tick (250ms)
    Tick,
    /// New snapshot from orchestrator
    Snapshot(DashboardSnapshot),
}

/// Run the event loop: merge keyboard events, ticks, and snapshot channel
/// into a single AppEvent stream.
pub async fn run_event_loop(
    event_tx: mpsc::Sender<AppEvent>,
    mut snapshot_rx: mpsc::Receiver<DashboardSnapshot>,
) {
    let mut reader = EventStream::new();
    let mut tick = interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            // Keyboard / terminal events
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        if event_tx.send(AppEvent::Key(key)).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(_)) | None => break,
                    _ => {} // Ignore resize, mouse, etc.
                }
            }
            // Render tick
            _ = tick.tick() => {
                if event_tx.send(AppEvent::Tick).await.is_err() {
                    break;
                }
            }
            // Snapshot from orchestrator
            maybe_snapshot = snapshot_rx.recv() => {
                match maybe_snapshot {
                    Some(snap) => {
                        if event_tx.send(AppEvent::Snapshot(snap)).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }
}
