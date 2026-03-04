use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::AppConfig;

/// Thread-safe shared state accessible by all modules
#[derive(Clone)]
pub struct SharedState {
    pub market_states: Arc<DashMap<String, MarketState>>,
    pub my_orders: Arc<DashMap<String, OrderRecord>>,
    pub positions: Arc<DashMap<String, PositionRecord>>,
    pub ws_last_message: Arc<RwLock<DateTime<Utc>>>,
    pub price_history: Arc<DashMap<String, Vec<PricePoint>>>,
    /// Mapping: token_id → market_id (WS uses token_id, rest of system uses market_id)
    pub token_to_market: Arc<DashMap<String, String>>,
    /// Daily realized PnL tracker (reset at UTC midnight)
    pub daily_pnl: Arc<RwLock<PnlTracker>>,
    /// Cached market settlement times (market_id → end_date)
    pub settlement_times: Arc<DashMap<String, DateTime<Utc>>>,
}

/// Tracks realized PnL from trade events
#[derive(Debug, Clone)]
pub struct PnlTracker {
    /// Cumulative realized PnL for the current day (USDC)
    pub realized_pnl: Decimal,
    /// Day this tracker is for
    pub date: chrono::NaiveDate,
    /// Number of fills tracked
    pub fill_count: u64,
}

impl PnlTracker {
    pub fn new() -> Self {
        Self {
            realized_pnl: Decimal::ZERO,
            date: Utc::now().date_naive(),
            fill_count: 0,
        }
    }

    /// Record a fill. For maker: BUY → spent USDC (negative), SELL → received USDC (positive)
    pub fn record_fill(&mut self, side: OrderSide, price: Decimal, size: Decimal) {
        let today = Utc::now().date_naive();
        if today != self.date {
            // Day rolled over — reset
            self.realized_pnl = Decimal::ZERO;
            self.date = today;
            self.fill_count = 0;
        }

        let cash_flow = price * size;
        match side {
            OrderSide::Buy => self.realized_pnl -= cash_flow,  // spent USDC
            OrderSide::Sell => self.realized_pnl += cash_flow, // received USDC
        }
        self.fill_count += 1;
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MarketState {
    pub market_id: String,
    pub midpoint: Decimal,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub spread: Decimal,
    pub last_trade_price: Option<Decimal>,
    pub updated_at: DateTime<Utc>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct OrderRecord {
    pub order_id: String,
    pub market_id: String,
    pub price: Decimal,
    pub size: Decimal,
    pub side: OrderSide,
    pub status: OrderStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

impl std::fmt::Display for OrderSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Buy => write!(f, "BUY"),
            Self::Sell => write!(f, "SELL"),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Pending,
    Live,
    Matched,
    Canceled,
}

#[derive(Debug, Clone)]
pub struct PositionRecord {
    pub market_id: String,
    pub yes_shares: Decimal,
    pub no_shares: Decimal,
    /// USDC-denominated value of YES position
    pub yes_value: Decimal,
    /// USDC-denominated value of NO position
    pub no_value: Decimal,
    pub updated_at: DateTime<Utc>,
}

impl PositionRecord {
    /// Inventory Imbalance Ratio: (yes_value - no_value) / (yes_value + no_value)
    /// Range: [-1.0, +1.0], positive means holding too much YES
    pub fn iir(&self) -> Decimal {
        let total = self.yes_value + self.no_value;
        if total.is_zero() {
            return Decimal::ZERO;
        }
        (self.yes_value - self.no_value) / total
    }

    /// Minimum of YES and NO shares (available for merge)
    pub fn mergeable_amount(&self) -> Decimal {
        self.yes_shares.min(self.no_shares)
    }
}

#[derive(Debug, Clone)]
pub struct PricePoint {
    pub price: Decimal,
    pub timestamp: DateTime<Utc>,
}

impl SharedState {
    pub fn new(config: &AppConfig) -> Self {
        let market_states = Arc::new(DashMap::new());
        let positions = Arc::new(DashMap::new());
        let token_to_market = Arc::new(DashMap::new());

        // Pre-populate markets and build token→market mapping
        for market in &config.markets {
            market_states.insert(
                market.market_id.clone(),
                MarketState {
                    market_id: market.market_id.clone(),
                    midpoint: Decimal::new(50, 2), // default 0.50
                    best_bid: None,
                    best_ask: None,
                    spread: Decimal::ZERO,
                    last_trade_price: None,
                    updated_at: Utc::now(),
                },
            );
            positions.insert(
                market.market_id.clone(),
                PositionRecord {
                    market_id: market.market_id.clone(),
                    yes_shares: Decimal::ZERO,
                    no_shares: Decimal::ZERO,
                    yes_value: Decimal::ZERO,
                    no_value: Decimal::ZERO,
                    updated_at: Utc::now(),
                },
            );
            token_to_market.insert(market.token_id.clone(), market.market_id.clone());
        }

        Self {
            market_states,
            my_orders: Arc::new(DashMap::new()),
            positions,
            ws_last_message: Arc::new(RwLock::new(Utc::now())),
            price_history: Arc::new(DashMap::new()),
            token_to_market,
            daily_pnl: Arc::new(RwLock::new(PnlTracker::new())),
            settlement_times: Arc::new(DashMap::new()),
        }
    }

    /// Resolve a token_id to the corresponding market_id
    pub fn resolve_market_id(&self, token_id: &str) -> Option<String> {
        self.token_to_market.get(token_id).map(|v| v.clone())
    }

    /// Get 5-minute price change for a market
    pub fn price_change_5min(&self, market_id: &str) -> Decimal {
        let Some(history) = self.price_history.get(market_id) else {
            return Decimal::ZERO;
        };

        let now = Utc::now();
        let five_min_ago = now - chrono::Duration::minutes(5);

        let recent: Vec<&PricePoint> = history
            .iter()
            .filter(|p| p.timestamp >= five_min_ago)
            .collect();

        if recent.len() < 2 {
            return Decimal::ZERO;
        }

        let first = recent.first().unwrap().price;
        let last = recent.last().unwrap().price;

        if first.is_zero() {
            Decimal::ZERO
        } else {
            (last - first) / first
        }
    }

    /// Record a new price point
    pub fn record_price(&self, market_id: &str, price: Decimal) {
        self.price_history
            .entry(market_id.to_string())
            .or_default()
            .push(PricePoint {
                price,
                timestamp: Utc::now(),
            });

        // Keep only last 30 minutes of history
        let cutoff = Utc::now() - chrono::Duration::minutes(30);
        if let Some(mut history) = self.price_history.get_mut(market_id) {
            history.retain(|p| p.timestamp >= cutoff);
        }
    }

    /// Seconds since last WebSocket message
    pub async fn ws_disconnect_secs(&self) -> u64 {
        let last = *self.ws_last_message.read().await;
        (Utc::now() - last).num_seconds().max(0) as u64
    }
}
