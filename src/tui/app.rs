use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use crate::config::MarketConfig;
use crate::strategy::PricingOverrides;

use super::input::{FormField, TextInput};
use super::snapshot::DashboardSnapshot;

/// TUI commands sent back to the orchestrator
#[derive(Debug)]
pub enum TuiCommand {
    Quit,
    RecoverL3,
    ToggleMarket { market_id: String },
    UpdateStrategy {
        market_id: String,
        profile_name: Option<String>,
        overrides: Option<PricingOverrides>,
        capital: Option<Decimal>,
    },
    AddMarket {
        market: MarketConfig,
        profile_name: String,
        capital: Decimal,
    },
    RemoveMarket {
        market_id: String,
    },
    /// Trigger async search via Gamma API; results come back via snapshot.
    SearchMarkets {
        query: String,
    },
}

/// Active tab
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Orders,
    Risk,
    Charts,
    Strategy,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Overview,
        Tab::Orders,
        Tab::Risk,
        Tab::Charts,
        Tab::Strategy,
    ];

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Orders => "Orders",
            Tab::Risk => "Risk",
            Tab::Charts => "Charts",
            Tab::Strategy => "Strategy",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            Tab::Overview => 0,
            Tab::Orders => 1,
            Tab::Risk => 2,
            Tab::Charts => 3,
            Tab::Strategy => 4,
        }
    }
}

/// Order filter for the Orders tab
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderFilter {
    All,
    LiveOnly,
}

/// Modal state — when active, captures all keyboard input.
#[derive(Debug, Clone)]
pub enum ModalState {
    None,
    /// Parameter editing modal for a specific market
    EditParams {
        market_id: String,
        market_name: String,
        fields: Vec<FormField>,
        cursor: usize,
        editing: Option<TextInput>,
    },
    /// Profile selection modal
    SelectProfile {
        market_id: String,
        profiles: Vec<String>,
        cursor: usize,
    },
    /// Confirm action (e.g., reset overrides, delete market)
    Confirm {
        title: String,
        message: String,
        action: ConfirmAction,
        selected_yes: bool,
    },
    /// Market search modal with text input and results
    SearchMarket {
        query: TextInput,
        results: Vec<SearchResultItem>,
        cursor: usize,
        searching: bool,
    },
}

/// A search result displayed in the SearchMarket modal
#[derive(Debug, Clone)]
pub struct SearchResultItem {
    pub condition_id: String,
    pub token_id: String,
    pub question: String,
    pub midpoint: Option<Decimal>,
    pub rewards_max_spread: Option<Decimal>,
    pub rewards_min_size: Option<Decimal>,
}

/// Actions that require confirmation
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    ResetOverrides { market_id: String },
    RemoveMarket { market_id: String, market_name: String },
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

    // Overview tab state: selected market for toggle
    pub selected_overview_market: usize,

    // Strategy tab state
    pub selected_strategy_market: usize,

    // Modal state
    pub modal: ModalState,
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
            selected_overview_market: 0,
            selected_strategy_market: 0,
            modal: ModalState::None,
        }
    }

    /// Is any modal currently active?
    pub fn modal_active(&self) -> bool {
        !matches!(self.modal, ModalState::None)
    }

    /// Handle a key event, returning whether the screen needs redraw
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Modal captures all input when active
        if self.modal_active() {
            return self.handle_modal_key(key);
        }

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
            KeyCode::Char('5') => {
                self.current_tab = Tab::Strategy;
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
            Tab::Overview => self.handle_overview_key(key),
            Tab::Orders => self.handle_orders_key(key),
            Tab::Risk => self.handle_risk_key(key),
            Tab::Charts => self.handle_charts_key(key),
            Tab::Strategy => self.handle_strategy_key(key),
        }
    }

    fn handle_overview_key(&mut self, key: KeyEvent) -> bool {
        let market_count = self
            .snapshot
            .as_ref()
            .map(|s| s.markets.len())
            .unwrap_or(0);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if market_count > 0 {
                    self.selected_overview_market =
                        (self.selected_overview_market + 1).min(market_count - 1);
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected_overview_market = self.selected_overview_market.saturating_sub(1);
                true
            }
            KeyCode::Char('e') => {
                // Toggle enable/disable for selected market
                if let Some(snap) = &self.snapshot {
                    if let Some(market) = snap.markets.get(self.selected_overview_market) {
                        let market_id = market.market_id.clone();
                        let tx = self.cmd_tx.clone();
                        tokio::spawn(async move {
                            let _ = tx.send(TuiCommand::ToggleMarket { market_id }).await;
                        });
                    }
                }
                true
            }
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

    fn handle_strategy_key(&mut self, key: KeyEvent) -> bool {
        let market_count = self
            .snapshot
            .as_ref()
            .map(|s| s.markets.len())
            .unwrap_or(0);

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if market_count > 0 {
                    self.selected_strategy_market =
                        (self.selected_strategy_market + 1).min(market_count - 1);
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected_strategy_market = self.selected_strategy_market.saturating_sub(1);
                true
            }
            KeyCode::Char('e') => {
                // Toggle enable/disable for selected market
                if let Some(snap) = &self.snapshot {
                    if let Some(market) = snap.markets.get(self.selected_strategy_market) {
                        let market_id = market.market_id.clone();
                        let tx = self.cmd_tx.clone();
                        tokio::spawn(async move {
                            let _ = tx.send(TuiCommand::ToggleMarket { market_id }).await;
                        });
                    }
                }
                true
            }
            KeyCode::Enter => {
                // Open parameter editing modal
                self.open_edit_params_modal();
                true
            }
            KeyCode::Char('p') => {
                // Open profile selection modal
                self.open_profile_modal();
                true
            }
            KeyCode::Char('r') => {
                // Reset overrides → confirm dialog
                if let Some(snap) = &self.snapshot {
                    if let Some(market) = snap.markets.get(self.selected_strategy_market) {
                        self.modal = ModalState::Confirm {
                            title: "Reset Overrides".to_string(),
                            message: format!("Reset all overrides for '{}'?", market.name),
                            action: ConfirmAction::ResetOverrides {
                                market_id: market.market_id.clone(),
                            },
                            selected_yes: false,
                        };
                    }
                }
                true
            }
            KeyCode::Char('a') => {
                // Open search modal to add a new market
                self.modal = ModalState::SearchMarket {
                    query: TextInput::new("Search", "", false),
                    results: Vec::new(),
                    cursor: 0,
                    searching: false,
                };
                true
            }
            KeyCode::Char('d') => {
                // Delete selected market → confirm dialog
                if let Some(snap) = &self.snapshot {
                    if let Some(market) = snap.markets.get(self.selected_strategy_market) {
                        self.modal = ModalState::Confirm {
                            title: "Remove Market".to_string(),
                            message: format!(
                                "Remove '{}' and cancel all its orders?",
                                market.name
                            ),
                            action: ConfirmAction::RemoveMarket {
                                market_id: market.market_id.clone(),
                                market_name: market.name.clone(),
                            },
                            selected_yes: false,
                        };
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Open the EditParams modal for the currently selected strategy market.
    fn open_edit_params_modal(&mut self) {
        let Some(snap) = &self.snapshot else {
            return;
        };
        let Some(market) = snap.markets.get(self.selected_strategy_market) else {
            return;
        };

        // Build form fields from the snapshot's pricing factors
        // These show current effective values; user edits create overrides.
        let fields = vec![
            FormField::decimal("base_half_spread", &format!("{}", market.vaf)),
            FormField::decimal("skew_factor", &format!("{}", market.skew.abs())),
            FormField::decimal("capital", &format!("{}", market.capital_allocation)),
        ];

        self.modal = ModalState::EditParams {
            market_id: market.market_id.clone(),
            market_name: market.name.clone(),
            fields,
            cursor: 0,
            editing: None,
        };
    }

    /// Open the Profile selection modal.
    fn open_profile_modal(&mut self) {
        let Some(snap) = &self.snapshot else {
            return;
        };
        let Some(market) = snap.markets.get(self.selected_strategy_market) else {
            return;
        };

        // Profile names from the snapshot (populated by StrategyRegistry)
        let profiles = if snap.profile_names.is_empty() {
            vec!["default".to_string()]
        } else {
            snap.profile_names.clone()
        };

        let current_idx = profiles
            .iter()
            .position(|p| *p == market.profile_name)
            .unwrap_or(0);

        self.modal = ModalState::SelectProfile {
            market_id: market.market_id.clone(),
            profiles,
            cursor: current_idx,
        };
    }

    /// Handle keyboard events when a modal is active.
    fn handle_modal_key(&mut self, key: KeyEvent) -> bool {
        // Escape always closes the modal
        if key.code == KeyCode::Esc {
            // If editing a field, cancel editing first
            if let ModalState::EditParams { editing, .. } = &self.modal {
                if editing.is_some() {
                    if let ModalState::EditParams {
                        ref mut editing, ..
                    } = self.modal
                    {
                        *editing = None;
                        return true;
                    }
                }
            }
            self.modal = ModalState::None;
            return true;
        }

        match &self.modal {
            ModalState::None => false,
            ModalState::EditParams { .. } => self.handle_edit_params_key(key),
            ModalState::SelectProfile { .. } => self.handle_select_profile_key(key),
            ModalState::Confirm { .. } => self.handle_confirm_key(key),
            ModalState::SearchMarket { .. } => self.handle_search_market_key(key),
        }
    }

    fn handle_edit_params_key(&mut self, key: KeyEvent) -> bool {
        let ModalState::EditParams {
            ref market_id,
            ref mut fields,
            ref mut cursor,
            ref mut editing,
            ..
        } = self.modal
        else {
            return false;
        };

        // If actively editing a field
        if let Some(input) = editing {
            match key.code {
                KeyCode::Enter => {
                    // Save the edited value back to the field
                    if let Some(field) = fields.get_mut(*cursor) {
                        field.value = input.value.clone();
                    }
                    *editing = None;
                    return true;
                }
                KeyCode::Esc => {
                    *editing = None;
                    return true;
                }
                _ => {
                    return input.handle_key(key);
                }
            }
        }

        // Not editing — navigate or start editing
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                *cursor = (*cursor + 1).min(fields.len().saturating_sub(1));
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *cursor = cursor.saturating_sub(1);
                true
            }
            KeyCode::Enter => {
                // Start editing the selected field
                if let Some(field) = fields.get(*cursor) {
                    *editing = Some(TextInput::new(
                        &field.label,
                        &field.value,
                        field.is_numeric(),
                    ));
                }
                true
            }
            KeyCode::Char('s') => {
                // Save all fields and send UpdateStrategy command
                let mid = market_id.clone();
                let overrides = self.build_overrides_from_fields();
                let tx = self.cmd_tx.clone();
                tokio::spawn(async move {
                    let _ = tx
                        .send(TuiCommand::UpdateStrategy {
                            market_id: mid,
                            profile_name: None,
                            overrides: Some(overrides),
                            capital: None,
                        })
                        .await;
                });
                self.modal = ModalState::None;
                true
            }
            _ => false,
        }
    }

    fn handle_select_profile_key(&mut self, key: KeyEvent) -> bool {
        let ModalState::SelectProfile {
            ref market_id,
            ref profiles,
            ref mut cursor,
        } = self.modal
        else {
            return false;
        };

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                *cursor = (*cursor + 1).min(profiles.len().saturating_sub(1));
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *cursor = cursor.saturating_sub(1);
                true
            }
            KeyCode::Enter => {
                // Apply selected profile
                if let Some(profile_name) = profiles.get(*cursor) {
                    let mid = market_id.clone();
                    let pname = profile_name.clone();
                    let tx = self.cmd_tx.clone();
                    tokio::spawn(async move {
                        let _ = tx
                            .send(TuiCommand::UpdateStrategy {
                                market_id: mid,
                                profile_name: Some(pname),
                                overrides: None,
                                capital: None,
                            })
                            .await;
                    });
                }
                self.modal = ModalState::None;
                true
            }
            _ => false,
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> bool {
        let ModalState::Confirm {
            ref action,
            ref mut selected_yes,
            ..
        } = self.modal
        else {
            return false;
        };

        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                // Execute the confirmed action
                let action = action.clone();
                self.execute_confirm_action(action);
                self.modal = ModalState::None;
                true
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.modal = ModalState::None;
                true
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                *selected_yes = !*selected_yes;
                true
            }
            KeyCode::Enter => {
                if *selected_yes {
                    let action = action.clone();
                    self.execute_confirm_action(action);
                }
                self.modal = ModalState::None;
                true
            }
            _ => false,
        }
    }

    fn handle_search_market_key(&mut self, key: KeyEvent) -> bool {
        let ModalState::SearchMarket {
            ref mut query,
            ref mut results,
            ref mut cursor,
            ref mut searching,
        } = self.modal
        else {
            return false;
        };

        match key.code {
            KeyCode::Enter => {
                if results.is_empty() || *searching {
                    // Trigger search with current query
                    if !query.value.is_empty() {
                        let q = query.value.clone();
                        let tx = self.cmd_tx.clone();
                        *searching = true;
                        tokio::spawn(async move {
                            let _ = tx.send(TuiCommand::SearchMarkets { query: q }).await;
                        });
                    }
                } else if let Some(result) = results.get(*cursor) {
                    // Add selected market
                    let market = MarketConfig {
                        market_id: result.condition_id.clone(),
                        token_id: result.token_id.clone(),
                        name: result.question.clone(),
                        max_incentive_spread: result
                            .rewards_max_spread
                            .unwrap_or(Decimal::new(3, 2)), // default 0.03
                        min_size: result
                            .rewards_min_size
                            .unwrap_or(Decimal::new(5, 0)), // default 5
                    };
                    let tx = self.cmd_tx.clone();
                    tokio::spawn(async move {
                        let _ = tx
                            .send(TuiCommand::AddMarket {
                                market,
                                profile_name: "default".to_string(),
                                capital: Decimal::new(200, 0), // default $200
                            })
                            .await;
                    });
                    self.modal = ModalState::None;
                }
                true
            }
            KeyCode::Char('j') | KeyCode::Down if !results.is_empty() => {
                *cursor = (*cursor + 1).min(results.len().saturating_sub(1));
                true
            }
            KeyCode::Char('k') | KeyCode::Up if !results.is_empty() => {
                *cursor = cursor.saturating_sub(1);
                true
            }
            _ => {
                // Forward to text input
                query.handle_key(key)
            }
        }
    }

    fn execute_confirm_action(&self, action: ConfirmAction) {
        match action {
            ConfirmAction::ResetOverrides { market_id } => {
                let tx = self.cmd_tx.clone();
                tokio::spawn(async move {
                    let _ = tx
                        .send(TuiCommand::UpdateStrategy {
                            market_id,
                            profile_name: None,
                            overrides: Some(PricingOverrides::default()),
                            capital: None,
                        })
                        .await;
                });
            }
            ConfirmAction::RemoveMarket { market_id, .. } => {
                let tx = self.cmd_tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(TuiCommand::RemoveMarket { market_id }).await;
                });
            }
        }
    }

    /// Update search results from snapshot (called when orchestrator sends results back)
    pub fn update_search_results(&mut self, results: Vec<SearchResultItem>) {
        if let ModalState::SearchMarket {
            results: ref mut modal_results,
            ref mut cursor,
            ref mut searching,
            ..
        } = self.modal
        {
            *modal_results = results;
            *cursor = 0;
            *searching = false;
        }
    }

    /// Build PricingOverrides from the current edit form fields.
    fn build_overrides_from_fields(&self) -> PricingOverrides {
        let ModalState::EditParams { ref fields, .. } = self.modal else {
            return PricingOverrides::default();
        };

        let mut overrides = PricingOverrides::default();

        for field in fields {
            let val: Option<Decimal> = field.value.parse().ok();
            match field.label.as_str() {
                "base_half_spread" => overrides.base_half_spread = val,
                "skew_factor" => overrides.skew_factor = val,
                _ => {}
            }
        }

        overrides
    }
}
