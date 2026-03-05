use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::AppConfig;
use crate::data::state::OrderStatus;
use crate::data::SharedState;
use crate::risk::{RiskController, RiskLevel};

/// Immutable snapshot of the entire dashboard state.
/// Sent from the orchestrator to the TUI task via mpsc channel.
/// The TUI never directly accesses DashMap/RwLock — only reads this struct.
#[derive(Debug, Clone)]
pub struct DashboardSnapshot {
    pub timestamp: DateTime<Utc>,
    pub risk_level: RiskLevel,
    pub ghost_fill_count: u32,
    pub l2_entered_at: Option<DateTime<Utc>>,
    pub daily_pnl: Decimal,
    pub fill_count: u64,
    pub market_ws_connected: bool,
    pub user_ws_connected: bool,
    pub ws_disconnect_secs: u64,
    pub markets: Vec<MarketSnapshot>,
    pub orders: Vec<OrderSnapshot>,
    pub price_histories: Vec<PriceHistorySnapshot>,
}

#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    pub market_id: String,
    pub name: String,
    pub midpoint: Decimal,
    pub spread: Decimal,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub iir: Decimal,
    pub yes_shares: Decimal,
    pub no_shares: Decimal,
    pub yes_value: Decimal,
    pub no_value: Decimal,
    pub active_orders: usize,
}

#[derive(Debug, Clone)]
pub struct OrderSnapshot {
    pub order_id: String,
    pub market_id: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub status: String,
    pub age_secs: i64,
}

#[derive(Debug, Clone)]
pub struct PriceHistorySnapshot {
    pub market_id: String,
    /// (timestamp, price as f64) for Chart widget
    pub points: Vec<(f64, f64)>,
}

/// Collect a snapshot from SharedState + RiskController.
/// Called by the orchestrator on each tui_tick (250ms).
pub async fn collect_snapshot(
    config: &AppConfig,
    state: &SharedState,
    risk_controller: &Arc<Mutex<RiskController>>,
) -> DashboardSnapshot {
    let now = Utc::now();

    // Risk info (single lock acquisition)
    let (risk_level, ghost_fill_count, l2_entered_at) = {
        let rc = risk_controller.lock().await;
        (rc.level(), rc.ghost_fill_count(), rc.l2_entered_at())
    };

    // PnL info
    let (daily_pnl, fill_count) = {
        let pnl = state.daily_pnl.read().await;
        (pnl.realized_pnl, pnl.fill_count)
    };

    // WS status
    let market_ws_connected = state.market_ws_connected.load(std::sync::atomic::Ordering::Acquire);
    let user_ws_connected = state.user_ws_connected.load(std::sync::atomic::Ordering::Acquire);
    let ws_disconnect_secs = state.max_ws_disconnect_secs().await;

    // Market snapshots
    let mut markets = Vec::with_capacity(config.markets.len());
    for market_cfg in &config.markets {
        let mid = &market_cfg.market_id;

        let (midpoint, spread, best_bid, best_ask) = state
            .market_states
            .get(mid)
            .map(|ms| (ms.midpoint, ms.spread, ms.best_bid, ms.best_ask))
            .unwrap_or((Decimal::ZERO, Decimal::ZERO, None, None));

        let (iir, yes_shares, no_shares, yes_value, no_value) = state
            .positions
            .get(mid)
            .map(|p| (p.iir(), p.yes_shares, p.no_shares, p.yes_value, p.no_value))
            .unwrap_or((Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO));

        let active_orders = state
            .my_orders
            .iter()
            .filter(|o| o.market_id == *mid && matches!(o.status, OrderStatus::Live))
            .count();

        markets.push(MarketSnapshot {
            market_id: mid.clone(),
            name: market_cfg.name.clone(),
            midpoint,
            spread,
            best_bid,
            best_ask,
            iir,
            yes_shares,
            no_shares,
            yes_value,
            no_value,
            active_orders,
        });
    }

    // Order snapshots
    let mut orders: Vec<OrderSnapshot> = state
        .my_orders
        .iter()
        .map(|entry| {
            let o = entry.value();
            OrderSnapshot {
                order_id: o.order_id.clone(),
                market_id: o.market_id.clone(),
                side: o.side.to_string(),
                price: o.price,
                size: o.size,
                status: format!("{:?}", o.status),
                age_secs: (now - o.created_at).num_seconds(),
            }
        })
        .collect();
    // Sort: Live first, then by age descending
    orders.sort_by(|a, b| a.status.cmp(&b.status).then(b.age_secs.cmp(&a.age_secs)));

    // Price history snapshots (last 60 minutes)
    let base_ts = now.timestamp() as f64;
    let mut price_histories = Vec::new();
    for market_cfg in &config.markets {
        let mid = &market_cfg.market_id;
        if let Some(history) = state.price_history.get(mid) {
            let points: Vec<(f64, f64)> = history
                .iter()
                .map(|p| {
                    let x = (p.timestamp.timestamp() as f64) - base_ts + 3600.0;
                    let y = p.price.to_string().parse::<f64>().unwrap_or(0.0);
                    (x, y)
                })
                .collect();
            if !points.is_empty() {
                price_histories.push(PriceHistorySnapshot {
                    market_id: mid.clone(),
                    points,
                });
            }
        }
    }

    DashboardSnapshot {
        timestamp: now,
        risk_level,
        ghost_fill_count,
        l2_entered_at,
        daily_pnl,
        fill_count,
        market_ws_connected,
        user_ws_connected,
        ws_disconnect_secs,
        markets,
        orders,
        price_histories,
    }
}
