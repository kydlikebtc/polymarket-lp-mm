use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use alloy::primitives::{Address, U256};
use anyhow::Result;
use chrono::Utc;
use futures::StreamExt;
use rust_decimal::Decimal;
use tokio::time::Duration;
use tracing::{debug, error, info, warn};

use polymarket_client_sdk::auth::Credentials;
use polymarket_client_sdk::clob::types::OrderStatusType;
use polymarket_client_sdk::clob::ws::{self as sdk_ws, WsMessage};
use polymarket_client_sdk::clob::ws::types::response::OrderMessageType;

use super::SharedState;
use crate::data::state::OrderStatus;
use crate::risk::RiskController;

/// Run market data WebSocket using the SDK.
/// Subscribes to orderbook updates for all configured token IDs.
/// Updates SharedState with latest prices and spread.
pub async fn run_market_ws(
    state: SharedState,
    token_ids: Vec<String>,
) -> Result<()> {
    info!(
        "Market WebSocket starting for {} tokens",
        token_ids.len()
    );

    let asset_ids: Vec<U256> = token_ids
        .iter()
        .map(|id| U256::from_str(id))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("Invalid token_id: {e}"))?;

    let mut backoff_secs = 1u64;
    const MAX_BACKOFF_SECS: u64 = 30;

    loop {
        let was_connected = match run_market_ws_inner(&state, &asset_ids).await {
            Ok(connected) => {
                info!("Market WebSocket closed normally, reconnecting in {backoff_secs}s...");
                connected
            }
            Err(e) => {
                error!("Market WebSocket error: {e:#}, reconnecting in {backoff_secs}s...");
                false
            }
        };

        // R6-3: Reset connected flag on disconnect so strategy loop pauses until reconnected.
        // Uses Release ordering so the strategy thread (Acquire) sees it immediately.
        state.market_ws_connected.store(false, Ordering::Release);

        // Reset backoff on successful connection; exponential increase only on failure
        if was_connected {
            backoff_secs = 1;
        }
        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        if !was_connected {
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
        }
    }
}

/// Returns Ok(true) if at least one message was received (connection was live),
/// Ok(false) if closed before any data, Err on stream error.
async fn run_market_ws_inner(
    state: &SharedState,
    asset_ids: &[U256],
) -> Result<bool> {
    let client = sdk_ws::Client::default();
    let stream = client.subscribe_orderbook(asset_ids.to_vec())?;
    let mut stream = Box::pin(stream);

    info!("Market WebSocket connected, subscribed to {} assets", asset_ids.len());

    // Update WS heartbeat
    {
        let mut last_msg = state.ws_last_message.write().await;
        *last_msg = Utc::now();
    }

    let mut received_any = false;

    while let Some(result) = stream.next().await {
        match result {
            Ok(book) => {
                received_any = true;
                // R5-12: Mark market WS as having received data
                // R6-8: Use Release ordering for ARM correctness
                state.market_ws_connected.store(true, Ordering::Release);

                // Update heartbeat timestamp
                {
                    let mut last_msg = state.ws_last_message.write().await;
                    *last_msg = Utc::now();
                }

                let token_id = book.asset_id.to_string();

                // Resolve token_id → market_id for state lookup
                let Some(market_id) = state.resolve_market_id(&token_id) else {
                    debug!("Unknown token_id from WS: {token_id}, skipping");
                    continue;
                };

                // Extract best bid/ask from the book
                // R8-SEC10: Validate price range for binary markets [0.001, 0.999].
                // Rejects anomalous WS data that could poison midpoint/IIR calculations.
                let best_bid = book.bids.first().map(|b| b.price).filter(|p| {
                    *p > Decimal::ZERO && *p < Decimal::ONE
                });
                let best_ask = book.asks.first().map(|a| a.price).filter(|p| {
                    *p > Decimal::ZERO && *p < Decimal::ONE
                });

                // R6-6: Read midpoint and drop DashMap RefMut before calling record_price,
                // to avoid holding market_states entry across another DashMap access.
                let midpoint_for_record = if let Some(mut ms) = state.market_states.get_mut(&market_id) {
                    // Update whichever side(s) are available
                    if let Some(bid) = best_bid { ms.best_bid = Some(bid); }
                    if let Some(ask) = best_ask { ms.best_ask = Some(ask); }

                    match (ms.best_bid, ms.best_ask) {
                        (Some(bid), Some(ask)) => {
                            ms.midpoint = (bid + ask) / Decimal::TWO;
                            ms.spread = ask - bid;
                        }
                        (Some(bid), None) => {
                            ms.midpoint = bid;
                            ms.spread = Decimal::ZERO;
                        }
                        (None, Some(ask)) => {
                            ms.midpoint = ask;
                            ms.spread = Decimal::ZERO;
                        }
                        (None, None) => {}
                    }
                    ms.updated_at = Utc::now();
                    let mid = ms.midpoint;
                    let spread = ms.spread;

                    debug!(
                        "Book update: market={}, token={}, mid={}, spread={}, bids={}, asks={}",
                        market_id, token_id, mid, spread,
                        book.bids.len(), book.asks.len()
                    );

                    Some(mid)
                    // ms (RefMut) dropped here
                } else {
                    None
                };

                // Record price history AFTER releasing market_states entry
                if let Some(mid) = midpoint_for_record {
                    state.record_price(&market_id, mid);
                }
            }
            Err(e) => {
                warn!("Market WS stream error: {e}");
                return Err(e.into());
            }
        }
    }

    Ok(received_any)
}

/// Run user events WebSocket using the SDK.
/// Receives order placement/update/cancellation events.
/// Updates SharedState and detects ghost fills via RiskController.
pub async fn run_user_ws(
    state: SharedState,
    credentials: Credentials,
    address: Address,
    risk_controller: Arc<tokio::sync::Mutex<RiskController>>,
) -> Result<()> {
    debug!("User WebSocket starting for address={address}");

    let mut backoff_secs = 1u64;
    const MAX_BACKOFF_SECS: u64 = 30;

    loop {
        let was_connected = match run_user_ws_inner(&state, &credentials, address, &risk_controller).await {
            Ok(connected) => {
                info!("User WebSocket closed normally, reconnecting in {backoff_secs}s...");
                connected
            }
            Err(e) => {
                error!("User WebSocket error: {e:#}, reconnecting in {backoff_secs}s...");
                false
            }
        };

        // R6-3: Reset connected flag on disconnect
        state.user_ws_connected.store(false, Ordering::Release);

        if was_connected {
            backoff_secs = 1;
        }
        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        if !was_connected {
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
        }
    }
}

/// Returns Ok(true) if at least one message was received, Ok(false) otherwise.
async fn run_user_ws_inner(
    state: &SharedState,
    credentials: &Credentials,
    address: Address,
    risk_controller: &Arc<tokio::sync::Mutex<RiskController>>,
) -> Result<bool> {
    let client = sdk_ws::Client::default()
        .authenticate(credentials.clone(), address)?;

    // Subscribe to all markets (empty vec = all)
    let markets = Vec::new();
    let stream = client.subscribe_user_events(markets)?;
    let mut stream = std::pin::pin!(stream);

    info!("User WebSocket connected and authenticated");

    let mut received_any = false;

    while let Some(event) = stream.next().await {
        received_any = true;
        // R5-12 + R6-8: Mark user WS as having received data (Release for ARM)
        state.user_ws_connected.store(true, Ordering::Release);

        // Update user WS heartbeat for disconnect detection
        {
            let mut last_msg = state.user_ws_last_message.write().await;
            *last_msg = Utc::now();
        }

        match event {
            Ok(WsMessage::Order(order)) => {
                let order_id = order.id.to_string();
                debug!("User WS order event: id={order_id}, type={:?}", order.msg_type);

                // Update order state
                if let Some(mut our_order) = state.my_orders.get_mut(&order_id) {
                    // R5-19: Cross-validate that WS event matches our local record.
                    let ws_market = state.resolve_market_id(&order.asset_id.to_string());
                    if let Some(ref ws_mid) = ws_market {
                        if *ws_mid != our_order.market_id {
                            warn!(
                                "WS order event market mismatch: order={order_id}, \
                                 local={}, ws={ws_mid}. Ignoring.",
                                our_order.market_id
                            );
                            continue;
                        }
                    }

                    // Determine new status: prefer SDK `status` field, fall back to `msg_type`
                    let new_status = if let Some(ref sdk_status) = order.status {
                        match sdk_status {
                            OrderStatusType::Live => OrderStatus::Live,
                            OrderStatusType::Matched => OrderStatus::Matched,
                            OrderStatusType::Canceled => OrderStatus::Canceled,
                            _ => our_order.status,
                        }
                    } else {
                        match order.msg_type {
                            Some(OrderMessageType::Placement) => OrderStatus::Live,
                            Some(OrderMessageType::Cancellation) => OrderStatus::Canceled,
                            Some(OrderMessageType::Update) => OrderStatus::Matched,
                            _ => our_order.status,
                        }
                    };

                    our_order.status = new_status;
                    our_order.updated_at = Utc::now();

                    // Ghost fill detection: if order was cancelled but we didn't initiate it
                    if new_status == OrderStatus::Canceled {
                        let mut rc = risk_controller.lock().await;
                        if !rc.is_our_cancel(&order_id) {
                            warn!("Ghost fill detected! Order {order_id} cancelled without our request");
                            rc.record_ghost_fill();
                        }
                    }
                }
            }
            Ok(WsMessage::Trade(trade)) => {
                info!(
                    "FILL: id={}, market={}, side={:?}, price={}, size={}",
                    trade.id, trade.market, trade.side, trade.price, trade.size
                );

                // R8-SEC10: Validate trade price and size before processing
                if trade.price <= Decimal::ZERO || trade.price >= Decimal::ONE {
                    warn!(
                        "Trade {} has out-of-range price={}, skipping",
                        trade.id, trade.price
                    );
                    continue;
                }
                if trade.size <= Decimal::ZERO || trade.size > Decimal::from(1_000_000u64) {
                    warn!(
                        "Trade {} has out-of-range size={}, skipping",
                        trade.id, trade.size
                    );
                    continue;
                }

                // Map SDK side to our OrderSide
                let our_side = match trade.side {
                    polymarket_client_sdk::clob::types::Side::Buy => {
                        crate::data::state::OrderSide::Buy
                    }
                    polymarket_client_sdk::clob::types::Side::Sell => {
                        crate::data::state::OrderSide::Sell
                    }
                    other => {
                        warn!("Unknown trade side {:?} for trade {}, skipping", other, trade.id);
                        continue;
                    }
                };

                // Resolve market_id before recording PnL or updating position
                let Some(resolved) = state.resolve_market_id(&trade.asset_id.to_string()) else {
                    warn!(
                        "Cannot resolve market_id for trade asset_id={}, skipping PnL and position update",
                        trade.asset_id
                    );
                    continue;
                };

                // Record PnL with per-market cost basis
                {
                    let mut pnl = state.daily_pnl.write().await;
                    pnl.record_fill(&resolved, our_side, trade.price, trade.size);
                    debug!(
                        "PnL update: market={resolved}, realized={}, fills={}",
                        pnl.realized_pnl, pnl.fill_count
                    );
                }

                // Read midpoint BEFORE acquiring positions write lock
                let current_midpoint = state.market_states.get(&resolved)
                    .map(|ms| ms.midpoint);

                if let Some(mut pos) = state.positions.get_mut(&resolved) {
                    match our_side {
                        crate::data::state::OrderSide::Buy => {
                            pos.yes_shares += trade.size;
                        }
                        crate::data::state::OrderSide::Sell => {
                            pos.yes_shares = (pos.yes_shares - trade.size).max(Decimal::ZERO);
                        }
                    }
                    if let Some(mid) = current_midpoint {
                        pos.yes_value = pos.yes_shares * mid;
                        pos.no_value = pos.no_shares * (Decimal::ONE - mid);
                    }
                    pos.updated_at = Utc::now();
                }

                // Push PnL to risk controller
                {
                    let pnl = state.daily_pnl.read().await;
                    let mut rc = risk_controller.lock().await;
                    rc.update_pnl(pnl.realized_pnl);
                }
            }
            Ok(_other) => {
                // Other message types (heartbeat, etc.)
            }
            Err(e) => {
                warn!("User WS stream error: {e}");
                return Err(e.into());
            }
        }
    }

    Ok(received_any)
}
