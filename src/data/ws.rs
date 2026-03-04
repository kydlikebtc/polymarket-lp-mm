use anyhow::Result;
use chrono::Utc;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;
use tokio::time::{Duration, interval};
use tracing::{debug, info};

use super::SharedState;
use crate::data::state::OrderStatus;

/// WebSocket message types from Polymarket
#[derive(Debug, Deserialize)]
#[serde(tag = "event_type")]
pub enum WsMarketEvent {
    #[serde(rename = "book")]
    Book {
        market: String,
        #[serde(default)]
        bids: Vec<WsOrderLevel>,
        #[serde(default)]
        asks: Vec<WsOrderLevel>,
    },
    #[serde(rename = "price_change")]
    PriceChange {
        market: String,
        price: String,
    },
    #[serde(rename = "last_trade_price")]
    LastTradePrice {
        market: String,
        price: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct WsOrderLevel {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Deserialize)]
pub struct WsUserEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub order_id: Option<String>,
    pub market: Option<String>,
    pub side: Option<String>,
    pub price: Option<String>,
    pub size: Option<String>,
    pub status: Option<String>,
}

/// Process a market WebSocket event and update shared state
pub fn handle_market_event(state: &SharedState, event: &WsMarketEvent) {
    match event {
        WsMarketEvent::Book {
            market,
            bids,
            asks,
        } => {
            let best_bid = bids
                .first()
                .and_then(|b| Decimal::from_str(&b.price).ok());
            let best_ask = asks
                .first()
                .and_then(|a| Decimal::from_str(&a.price).ok());

            if let Some(mut ms) = state.market_states.get_mut(market) {
                if let (Some(bid), Some(ask)) = (best_bid, best_ask) {
                    ms.best_bid = Some(bid);
                    ms.best_ask = Some(ask);
                    ms.midpoint = (bid + ask) / Decimal::TWO;
                    ms.spread = ask - bid;
                    ms.updated_at = Utc::now();

                    // Record price for history
                    state.record_price(market, ms.midpoint);

                    debug!(
                        "Book update: market={market}, mid={}, spread={}",
                        ms.midpoint, ms.spread
                    );
                }
            }
        }
        WsMarketEvent::PriceChange { market, price } => {
            if let Ok(p) = Decimal::from_str(price) {
                if let Some(mut ms) = state.market_states.get_mut(market) {
                    ms.midpoint = p;
                    ms.updated_at = Utc::now();
                    state.record_price(market, p);
                    debug!("Price change: market={market}, price={p}");
                }
            }
        }
        WsMarketEvent::LastTradePrice { market, price } => {
            if let Ok(p) = Decimal::from_str(price) {
                if let Some(mut ms) = state.market_states.get_mut(market) {
                    ms.last_trade_price = Some(p);
                    ms.updated_at = Utc::now();
                }
            }
        }
    }
}

/// Process a user WebSocket event (order updates)
pub fn handle_user_event(
    state: &SharedState,
    event: &WsUserEvent,
    ghost_fill_callback: &mut dyn FnMut(&str),
) {
    let Some(order_id) = &event.order_id else {
        return;
    };

    match event.event_type.as_str() {
        "placement" | "update" => {
            let status = match event.status.as_deref() {
                Some("LIVE") => OrderStatus::Live,
                Some("MATCHED") => OrderStatus::Matched,
                Some("CANCELED") => OrderStatus::Canceled,
                _ => OrderStatus::Pending,
            };

            if let Some(mut order) = state.my_orders.get_mut(order_id) {
                order.status = status;
                order.updated_at = Utc::now();
                debug!("Order update: id={order_id}, status={status:?}");
            }

            // Ghost fill detection: unexpected cancellation
            if status == OrderStatus::Canceled {
                ghost_fill_callback(order_id);
            }
        }
        _ => {
            debug!("Unknown user event type: {}", event.event_type);
        }
    }
}

/// Placeholder for WebSocket connection management.
/// In the full implementation, this uses tokio-tungstenite with auto-reconnect.
pub async fn run_market_ws(
    state: SharedState,
    ws_url: String,
    market_ids: Vec<String>,
) -> Result<()> {
    info!(
        "Market WebSocket connecting to {ws_url} for {} markets",
        market_ids.len()
    );

    // TODO: Replace with actual tokio-tungstenite connection
    // For MVP, this is a stub that simulates the connection lifecycle
    //
    // The real implementation would:
    // 1. Connect to wss://ws-subscriptions-clob.polymarket.com/ws/market
    // 2. Send subscription: {"type":"subscribe","markets":["market_id"],"channels":["book","price_change"]}
    // 3. Process incoming messages via handle_market_event
    // 4. Send PING every 8 seconds
    // 5. Auto-reconnect on disconnect with exponential backoff

    let mut ping_interval = interval(Duration::from_secs(8));
    loop {
        ping_interval.tick().await;
        // In real impl: send PING frame
        let mut last_msg = state.ws_last_message.write().await;
        *last_msg = Utc::now();
    }
}

/// Placeholder for user WebSocket connection
pub async fn run_user_ws(
    _state: SharedState,
    ws_url: String,
    _api_key: String,
) -> Result<()> {
    info!("User WebSocket connecting to {ws_url}");

    // TODO: Replace with actual authenticated WebSocket connection
    // The real implementation would:
    // 1. Connect to wss://ws-subscriptions-clob.polymarket.com/ws/user
    // 2. Authenticate with API key
    // 3. Process order_update events via handle_user_event
    // 4. Maintain PING/PONG keepalive

    let mut ping_interval = interval(Duration::from_secs(8));
    loop {
        ping_interval.tick().await;
    }
}
