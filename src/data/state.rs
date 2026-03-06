use alloy::primitives::B256;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::RwLock;

use crate::config::AppConfig;

/// Thread-safe shared state accessible by all modules
#[derive(Clone)]
pub struct SharedState {
    pub market_states: Arc<DashMap<String, MarketState>>,
    pub my_orders: Arc<DashMap<String, OrderRecord>>,
    pub positions: Arc<DashMap<String, PositionRecord>>,
    /// Last message timestamp from market data WebSocket
    pub ws_last_message: Arc<RwLock<DateTime<Utc>>>,
    /// Last message timestamp from user events WebSocket
    pub user_ws_last_message: Arc<RwLock<DateTime<Utc>>>,
    pub price_history: Arc<DashMap<String, Vec<PricePoint>>>,
    /// Mapping: token_id → market_id (WS uses token_id, rest of system uses market_id)
    pub token_to_market: Arc<DashMap<String, String>>,
    /// Daily realized PnL tracker (reset at UTC midnight)
    pub daily_pnl: Arc<RwLock<PnlTracker>>,
    /// Cached market settlement times (market_id → end_date)
    pub settlement_times: Arc<DashMap<String, DateTime<Utc>>>,
    /// Cached market condition IDs (market_id → condition_id) for CTF merge operations
    pub condition_ids: Arc<DashMap<String, B256>>,
    /// R5-12: Whether market data WS has ever received a message
    pub market_ws_connected: Arc<AtomicBool>,
    /// R5-12: Whether user events WS has ever received a message
    pub user_ws_connected: Arc<AtomicBool>,
    /// USDC wallet balance (polled from REST API every 60s in position_tick)
    pub usdc_balance: Arc<RwLock<Decimal>>,
    /// Cumulative orders placed count (for execution stats display)
    pub orders_placed_count: Arc<AtomicU64>,
    /// Cumulative orders cancelled count (for execution stats display)
    pub orders_cancelled_count: Arc<AtomicU64>,
    /// Dynamic token list for WS subscription (updated when markets are added/removed).
    /// WS task reads this on each reconnection to subscribe to the latest set of tokens.
    pub ws_tokens: Arc<RwLock<Vec<String>>>,
    /// Signal to trigger WS reconnection (set to true when ws_tokens changes)
    pub ws_reconnect_needed: Arc<AtomicBool>,
}

/// Per-market cost basis for weighted-average PnL tracking.
#[derive(Debug, Clone)]
struct MarketCostBasis {
    avg_cost: Decimal,
    shares_held: Decimal,
}

/// Tracks realized PnL from trade events using per-market weighted-average cost basis.
///
/// Each market maintains an independent cost basis. Buy fills accumulate cost basis;
/// Sell fills realize PnL = (sell_price - avg_cost) * size against that market's basis.
///
/// R7-BL2: Known limitation — cost basis may drift over long runs due to:
/// 1. Startup seeding with approximate midpoint (0.50) for existing positions
/// 2. No periodic recalibration against actual on-chain balances
/// 3. Missed WS fill events (rare but possible during reconnection)
///
/// For MVP, this is acceptable since PnL is used for risk thresholds (relative),
/// not for accounting (absolute). Consider periodic recalibration in production.
#[derive(Debug, Clone)]
pub struct PnlTracker {
    /// Cumulative realized PnL for the current day across all markets (USDC)
    pub realized_pnl: Decimal,
    /// Day this tracker is for
    pub date: chrono::NaiveDate,
    /// Number of fills tracked
    pub fill_count: u64,
    /// Per-market cost basis (market_id → basis)
    market_bases: HashMap<String, MarketCostBasis>,
}

impl Default for PnlTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl PnlTracker {
    pub fn new() -> Self {
        Self {
            realized_pnl: Decimal::ZERO,
            date: Utc::now().date_naive(),
            fill_count: 0,
            market_bases: HashMap::new(),
        }
    }

    /// Record a fill using per-market weighted-average cost basis.
    ///
    /// BUY: increases position in market, updates that market's average cost.
    /// SELL: realizes PnL = (sell_price - market_avg_cost) * size.
    pub fn record_fill(&mut self, market_id: &str, side: OrderSide, price: Decimal, size: Decimal) {
        // R6-7: Guard against zero-size fills (prevent division by zero in avg_cost calc)
        if size.is_zero() {
            return;
        }

        let today = Utc::now().date_naive();
        if today != self.date {
            // Day rolled over — reset PnL but keep cost basis for overnight positions
            self.realized_pnl = Decimal::ZERO;
            self.date = today;
            self.fill_count = 0;
        }

        let basis = self.market_bases.entry(market_id.to_string()).or_insert(MarketCostBasis {
            avg_cost: Decimal::ZERO,
            shares_held: Decimal::ZERO,
        });

        match side {
            OrderSide::Buy => {
                let total_cost = basis.avg_cost * basis.shares_held + price * size;
                basis.shares_held += size;
                if basis.shares_held > Decimal::ZERO {
                    basis.avg_cost = total_cost / basis.shares_held;
                }
            }
            OrderSide::Sell => {
                let pnl = (price - basis.avg_cost) * size;
                self.realized_pnl += pnl;
                basis.shares_held = (basis.shares_held - size).max(Decimal::ZERO);
                if basis.shares_held.is_zero() {
                    basis.avg_cost = Decimal::ZERO;
                }
            }
        }
        self.fill_count += 1;
    }
}

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
    /// Allocated capital for this market (USDC), used for IIR normalization
    pub allocated_capital: Decimal,
    pub updated_at: DateTime<Utc>,
}

impl PositionRecord {
    /// Inventory Imbalance Ratio: net_exposure / allocated_capital
    /// Range: [-1.0, +1.0], positive means holding too much YES
    ///
    /// Uses capital-based normalization instead of (yes-no)/(yes+no)
    /// because MVP only trades YES tokens, so no_value is typically 0.
    /// The symmetric formula would always yield IIR=1.0 in that case.
    pub fn iir(&self) -> Decimal {
        if self.allocated_capital.is_zero() {
            return Decimal::ZERO;
        }
        let net_exposure = self.yes_value - self.no_value;
        let ratio = net_exposure / self.allocated_capital;
        // Clamp to [-1.0, +1.0]
        ratio.min(Decimal::ONE).max(-Decimal::ONE)
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

        let per_market_capital = config.per_market_capital();

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
                    allocated_capital: per_market_capital,
                    updated_at: Utc::now(),
                },
            );
            token_to_market.insert(market.token_id.clone(), market.market_id.clone());
        }

        let ws_tokens: Vec<String> = config.markets.iter().map(|m| m.token_id.clone()).collect();

        Self {
            market_states,
            my_orders: Arc::new(DashMap::new()),
            positions,
            ws_last_message: Arc::new(RwLock::new(Utc::now())),
            user_ws_last_message: Arc::new(RwLock::new(Utc::now())),
            price_history: Arc::new(DashMap::new()),
            token_to_market,
            daily_pnl: Arc::new(RwLock::new(PnlTracker::new())),
            settlement_times: Arc::new(DashMap::new()),
            condition_ids: Arc::new(DashMap::new()),
            market_ws_connected: Arc::new(AtomicBool::new(false)),
            user_ws_connected: Arc::new(AtomicBool::new(false)),
            usdc_balance: Arc::new(RwLock::new(Decimal::ZERO)),
            orders_placed_count: Arc::new(AtomicU64::new(0)),
            orders_cancelled_count: Arc::new(AtomicU64::new(0)),
            ws_tokens: Arc::new(RwLock::new(ws_tokens)),
            ws_reconnect_needed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Resolve a token_id to the corresponding market_id
    pub fn resolve_market_id(&self, token_id: &str) -> Option<String> {
        self.token_to_market.get(token_id).map(|v| v.clone())
    }

    /// Get 5-minute absolute price change for a market.
    /// R9-CR5: Uses max-min range (not first-last) to capture intra-window volatility.
    /// A price that spikes +10% then reverts should still register as high price change,
    /// not 0% (which first-last would report). This prevents risk under-reporting.
    ///
    /// Uses absolute change (not relative) because binary market prices near 0 or 1
    /// would cause tiny absolute movements to appear as huge relative changes.
    pub fn price_change_5min(&self, market_id: &str) -> Decimal {
        let Some(history) = self.price_history.get(market_id) else {
            return Decimal::ZERO;
        };

        let now = Utc::now();
        let five_min_ago = now - chrono::TimeDelta::minutes(5);

        let mut min_price = Decimal::MAX;
        let mut max_price = Decimal::ZERO;
        let mut count = 0u32;

        for point in history.iter().filter(|p| p.timestamp >= five_min_ago) {
            if point.price < min_price {
                min_price = point.price;
            }
            if point.price > max_price {
                max_price = point.price;
            }
            count += 1;
        }

        if count < 2 {
            return Decimal::ZERO;
        }

        max_price - min_price
    }

    /// Record a new price point (single lock: entry → push + retain)
    pub fn record_price(&self, market_id: &str, price: Decimal) {
        const MAX_PRICE_HISTORY: usize = 10_000;

        let mut entry = self.price_history
            .entry(market_id.to_string())
            .or_default();

        entry.push(PricePoint {
            price,
            timestamp: Utc::now(),
        });

        // Keep only last 60 minutes of history (matches compute_vaf 1-hour window)
        let cutoff = Utc::now() - chrono::TimeDelta::minutes(60);
        entry.retain(|p| p.timestamp >= cutoff);

        // Hard cap to prevent unbounded growth from high-frequency feeds
        if entry.len() > MAX_PRICE_HISTORY {
            let drain_count = entry.len() - MAX_PRICE_HISTORY;
            entry.drain(..drain_count);
        }
    }

    /// Seconds since last market data WS message
    pub async fn ws_disconnect_secs(&self) -> u64 {
        let last = *self.ws_last_message.read().await;
        (Utc::now() - last).num_seconds().max(0) as u64
    }

    /// Seconds since last user events WS message
    pub async fn user_ws_disconnect_secs(&self) -> u64 {
        let last = *self.user_ws_last_message.read().await;
        (Utc::now() - last).num_seconds().max(0) as u64
    }

    /// Disconnect duration for risk evaluation.
    ///
    /// Polymarket WS is push-based (data only on orderbook changes), so message
    /// recency alone cannot distinguish "quiet market" from "broken connection".
    /// Returns 0 when the market WS connected flag is true (connection alive),
    /// and actual elapsed seconds when disconnected.
    pub async fn max_ws_disconnect_secs(&self) -> u64 {
        if self.market_ws_connected.load(Ordering::Acquire) {
            0
        } else {
            self.ws_disconnect_secs().await
        }
    }

    /// R5-12: Check if both WebSocket connections have received at least one message.
    /// R6-8: Use Acquire ordering so that reads on ARM see the store from WS threads.
    pub fn both_ws_connected(&self) -> bool {
        self.market_ws_connected.load(Ordering::Acquire)
            && self.user_ws_connected.load(Ordering::Acquire)
    }

    /// Register a new market dynamically at runtime.
    ///
    /// Initializes all required state entries: market_states, positions,
    /// token_to_market mapping. Safe to call even if the market already exists
    /// (entries won't be overwritten if present).
    pub fn register_market(
        &self,
        market_id: &str,
        token_id: &str,
        allocated_capital: Decimal,
    ) {
        self.market_states.entry(market_id.to_string()).or_insert(
            MarketState {
                market_id: market_id.to_string(),
                midpoint: Decimal::new(50, 2),
                best_bid: None,
                best_ask: None,
                spread: Decimal::ZERO,
                last_trade_price: None,
                updated_at: Utc::now(),
            },
        );

        self.positions.entry(market_id.to_string()).or_insert(
            PositionRecord {
                market_id: market_id.to_string(),
                yes_shares: Decimal::ZERO,
                no_shares: Decimal::ZERO,
                yes_value: Decimal::ZERO,
                no_value: Decimal::ZERO,
                allocated_capital,
                updated_at: Utc::now(),
            },
        );

        self.token_to_market
            .insert(token_id.to_string(), market_id.to_string());
    }

    /// Unregister a market, removing all associated state entries.
    ///
    /// Note: does NOT cancel outstanding orders — caller must handle that first.
    pub fn unregister_market(&self, market_id: &str) {
        self.market_states.remove(market_id);
        self.positions.remove(market_id);
        self.price_history.remove(market_id);
        self.settlement_times.remove(market_id);
        self.condition_ids.remove(market_id);
        self.token_to_market
            .retain(|_token_id, mid| mid.as_str() != market_id);
    }
}
