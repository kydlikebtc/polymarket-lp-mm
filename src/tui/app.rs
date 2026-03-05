use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use super::snapshot::DashboardSnapshot;

/// TUI commands sent back to the orchestrator
#[derive(Debug)]
pub enum TuiCommand {
    Quit,
    RecoverL3,
}

/// Active tab
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Orders,
    Risk,
    Charts,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Overview, Tab::Orders, Tab::Risk, Tab::Charts];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Orders => "Orders",
            Tab::Risk => "Risk",
            Tab::Charts => "Charts",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            Tab::Overview => 0,
            Tab::Orders => 1,
            Tab::Risk => 2,
            Tab::Charts => 3,
        }
    }
}

/// Order filter for the Orders tab
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderFilter {
    All,
    LiveOnly,
}

/// Application state for the TUI
pub struct App {
    pub should_quit: bool,
    pub current_tab: Tab,
    pub snapshot: Option<DashboardSnapshot>,
    /// Command sender to orchestrator
    pub cmd_tx: mpsc::Sender<TuiCommand>,
    /// Uptime start
    pub started_at: chrono::DateTime<chrono::Utc>,

    // Orders tab state
    pub order_scroll: usize,
    pub order_filter: OrderFilter,

    // Charts tab state
    pub selected_market_index: usize,
}

impl App {
    pub fn new(cmd_tx: mpsc::Sender<TuiCommand>) -> Self {
        Self {
            should_quit: false,
            current_tab: Tab::Overview,
            snapshot: None,
            cmd_tx,
            started_at: chrono::Utc::now(),
            order_scroll: 0,
            order_filter: OrderFilter::All,
            selected_market_index: 0,
        }
    }

    /// Handle a key event, returning whether the screen needs redraw
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Global: quit
        if key.code == KeyCode::Char('q')
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.should_quit = true;
            let tx = self.cmd_tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(TuiCommand::Quit).await;
            });
            return true;
        }

        // Global: tab switching
        match key.code {
            KeyCode::Char('1') => {
                self.current_tab = Tab::Overview;
                return true;
            }
            KeyCode::Char('2') => {
                self.current_tab = Tab::Orders;
                return true;
            }
            KeyCode::Char('3') => {
                self.current_tab = Tab::Risk;
                return true;
            }
            KeyCode::Char('4') => {
                self.current_tab = Tab::Charts;
                return true;
            }
            KeyCode::Tab => {
                let next = (self.current_tab.index() + 1) % Tab::ALL.len();
                self.current_tab = Tab::ALL[next];
                return true;
            }
            KeyCode::BackTab => {
                let prev = (self.current_tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len();
                self.current_tab = Tab::ALL[prev];
                return true;
            }
            _ => {}
        }

        // Tab-specific keys
        match self.current_tab {
            Tab::Orders => self.handle_orders_key(key),
            Tab::Risk => self.handle_risk_key(key),
            Tab::Charts => self.handle_charts_key(key),
            _ => false,
        }
    }

    fn handle_orders_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.order_scroll = self.order_scroll.saturating_add(1);
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.order_scroll = self.order_scroll.saturating_sub(1);
                true
            }
            KeyCode::Char('f') => {
                self.order_filter = match self.order_filter {
                    OrderFilter::All => OrderFilter::LiveOnly,
                    OrderFilter::LiveOnly => OrderFilter::All,
                };
                self.order_scroll = 0;
                true
            }
            _ => false,
        }
    }

    fn handle_risk_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('r') {
            let tx = self.cmd_tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(TuiCommand::RecoverL3).await;
            });
            return true;
        }
        false
    }

    fn handle_charts_key(&mut self, key: KeyEvent) -> bool {
        let market_count = self
            .snapshot
            .as_ref()
            .map(|s| s.price_histories.len().max(1))
            .unwrap_or(1);
        match key.code {
            KeyCode::Left => {
                self.selected_market_index =
                    (self.selected_market_index + market_count - 1) % market_count;
                true
            }
            KeyCode::Right => {
                self.selected_market_index = (self.selected_market_index + 1) % market_count;
                true
            }
            _ => false,
        }
    }
}
