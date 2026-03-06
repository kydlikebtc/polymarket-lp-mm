//! R6-9: Market strategy logic extracted from monitor/mod.rs for maintainability.
//! Contains per-market strategy execution and risk evaluation.

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::{AppConfig, MarketConfig, PricingConfig};
use crate::data::SharedState;
use crate::data::gamma;
use crate::data::state::OrderSide;
use crate::execution::OrderExecutor;
use crate::pricing::{PricingEngine, QuoteOrder};
use crate::risk::{RiskController, RiskLevel};

/// Evaluate risk conditions and return current level
pub async fn evaluate_risk(
    controller: &Arc<Mutex<RiskController>>,
    state: &SharedState,
    config: &AppConfig,
) -> RiskLevel {
    let market_iirs: Vec<(String, Decimal)> = config
        .markets
        .iter()
        .map(|m| {
            let iir = state
                .positions
                .get(&m.market_id)
                .map(|p| p.iir())
                .unwrap_or(Decimal::ZERO);
            (m.market_id.clone(), iir)
        })
        .collect();

    let price_changes: Vec<(String, Decimal)> = config
        .markets
        .iter()
        .map(|m| {
            let change = state.price_change_5min(&m.market_id);
            (m.market_id.clone(), change)
        })
        .collect();

    let ws_disconnect = state.max_ws_disconnect_secs().await;

    // Sync daily PnL to risk controller before evaluation
    let daily_pnl = {
        let pnl = state.daily_pnl.read().await;
        pnl.realized_pnl
    };

    let mut rc = controller.lock().await;
    rc.update_pnl(daily_pnl);
    rc.evaluate(&market_iirs, &price_changes, ws_disconnect)
}

/// Run strategy for a single market.
///
/// R7-SEC5: Known TOCTOU window — between cancel confirmation (step 1) and
/// order submission (step 3), WS may disconnect or new fills may arrive.
/// The WS gap check (10s threshold) at step 2.5 mitigates stale-data risk,
/// but a brief window remains where conditions can change. This is inherent
/// to the cancel-then-quote architecture; a fully atomic approach would
/// require exchange-level replace-order support (not available on Polymarket).
pub async fn run_market_strategy(
    market_id: &str,
    market_config: &MarketConfig,
    pricing_config: &PricingConfig,
    state: &SharedState,
    pricing_engine: &PricingEngine,
    executor: &mut OrderExecutor,
    risk_controller: &Arc<Mutex<RiskController>>,
    risk_level: RiskLevel,
    per_market_capital: Decimal,
    last_midpoints: &mut HashMap<String, Decimal>,
    last_times: &mut HashMap<String, DateTime<Utc>>,
) -> Result<()> {

    let Some(ms) = state.market_states.get(market_id) else {
        return Ok(());
    };

    let current_midpoint = ms.midpoint;
    let now = Utc::now();

    // R8-BL7: If settlement time was never fetched from Gamma API, refuse to make market.
    // Without settlement time, TF defaults to 1.0 (normal), which could continue trading
    // on a settled or about-to-settle market, risking capital loss.
    if !state.settlement_times.contains_key(market_id) {
        warn!("Market {market_id}: no settlement time available, skipping (safety)");
        return Ok(());
    }

    // Check if we need to re-quote
    let should_requote = {
        let last_mid = last_midpoints.get(market_id).copied();
        let last_time = last_times.get(market_id).copied();

        let price_moved = last_mid
            .map(|lm| (current_midpoint - lm).abs() >= pricing_config.requote_threshold)
            .unwrap_or(true);

        let time_expired = last_time
            .map(|lt| {
                (now - lt).num_seconds() >= pricing_config.requote_interval_secs as i64
            })
            .unwrap_or(true);

        price_moved || time_expired
    };

    if !should_requote {
        return Ok(());
    }

    // R8-BL14: Refresh position values with current midpoint before reading IIR.
    // Without this, yes_value/no_value can be up to 60s stale (position_tick interval),
    // causing IIR to lag behind market moves between position ticks.
    // R9-CR7: Compute IIR inside the same get_mut block to avoid TOCTOU race
    // where a WS fill could modify positions between our write and read.
    let iir = if let Some(mut pos) = state.positions.get_mut(market_id) {
        pos.yes_value = pos.yes_shares * current_midpoint;
        pos.no_value = pos.no_shares * (Decimal::ONE - current_midpoint);
        pos.iir()
    } else {
        Decimal::ZERO
    };

    // Compute factors
    let vaf = pricing_engine.compute_vaf(state, market_id);
    // R5-17: Use .map() + unwrap_or(0.0) instead of .and_then() so that
    // a known but past settlement time yields Some(0.0) → TF=0.0 (stop trading),
    // while truly unknown settlement (no entry) yields None → TF=1.0 (normal).
    let hours_to_settlement = state
        .settlement_times
        .get(market_id)
        .map(|end_date| gamma::hours_until(&end_date).unwrap_or(0.0));
    let tf = pricing_engine.compute_tf(hours_to_settlement);

    // TF = 0 means stop market making
    if tf.is_zero() {
        info!("Market {market_id} approaching settlement, stopping");
        executor.cancel_market_orders(state, market_id, risk_controller).await?;
        // R8-BL5: Update tracking so subsequent ticks only re-check at requote_interval_secs,
        // avoiding repeated cancel attempts every 10s strategy tick.
        last_midpoints.insert(market_id.to_string(), current_midpoint);
        last_times.insert(market_id.to_string(), now);
        return Ok(());
    }

    // Step 1: Generate new quotes BEFORE cancelling, so we can compare with live orders.
    let available_yes_shares = state
        .positions
        .get(market_id)
        .map(|p| p.yes_shares)
        .unwrap_or(Decimal::ZERO);

    let orders = pricing_engine.generate_quotes(
        market_config,
        current_midpoint,
        iir,
        vaf,
        tf,
        per_market_capital,
        risk_level,
        available_yes_shares,
        pricing_config,
    );

    if orders.is_empty() {
        return Ok(());
    }

    // Step 2: Compare new quotes with existing live orders.
    // If all prices and sizes match, skip the cancel+place cycle to preserve
    // queue priority (price-time FIFO) and reduce ghost fill risk.
    let live_orders: Vec<(crate::data::state::OrderSide, Decimal, Decimal)> = state
        .my_orders
        .iter()
        .filter(|o| o.market_id == market_id && o.status == crate::data::state::OrderStatus::Live)
        .map(|o| (o.side, o.price, o.size))
        .collect();

    if quotes_match_live(&orders, &live_orders) {
        // Quotes unchanged — no need to cancel and re-place.
        // Update tracking timestamps to reset the requote timer.
        last_midpoints.insert(market_id.to_string(), current_midpoint);
        last_times.insert(market_id.to_string(), now);
        return Ok(());
    }

    // Step 3: Cancel existing orders (quotes differ, must replace)
    let cancelled_ids = executor.cancel_market_orders(state, market_id, risk_controller).await?;

    // Verify all cancels were confirmed via WS, not just locally timed out.
    if !cancelled_ids.is_empty() {
        let still_live = state.my_orders.iter()
            .filter(|o| o.market_id == market_id && o.status == crate::data::state::OrderStatus::Live)
            .count();
        if still_live > 0 {
            warn!(
                "Market {market_id} still has {still_live} orders not confirmed cancelled, skipping this round"
            );
            return Ok(());
        }
    }

    // Safety check: verify WS is still connected before submitting new orders.
    if !state.both_ws_connected() {
        warn!(
            "WS disconnected, skipping order submission for market={market_id}"
        );
        return Ok(());
    }

    // Estimate Q-Score for logging
    let estimated_q = pricing_engine.estimate_qscore(
        &orders,
        current_midpoint,
        market_config.max_incentive_spread,
    );
    info!(
        "Market {market_id}: mid={current_midpoint}, IIR={iir}, VAF={vaf}, TF={tf}, \
         orders={}, est_Q={estimated_q:.1}",
        orders.len()
    );

    // Step 4: Submit new orders
    executor.submit_orders(state, orders).await?;

    // Update tracking
    last_midpoints.insert(market_id.to_string(), current_midpoint);
    last_times.insert(market_id.to_string(), now);

    Ok(())
}

/// Check if new quotes match the currently live orders (same count, sides, prices, sizes).
/// Returns true if no cancel+replace is needed, preserving queue priority.
fn quotes_match_live(
    new_quotes: &[QuoteOrder],
    live_orders: &[(OrderSide, Decimal, Decimal)],
) -> bool {
    if new_quotes.len() != live_orders.len() {
        return false;
    }

    // Build sorted tuples for comparison: (side_is_buy, price, size)
    let mut new_set: Vec<(bool, Decimal, Decimal)> = new_quotes
        .iter()
        .map(|q| (matches!(q.side, OrderSide::Buy), q.price, q.size))
        .collect();
    new_set.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut live_set: Vec<(bool, Decimal, Decimal)> = live_orders
        .iter()
        .map(|(side, price, size)| (matches!(side, OrderSide::Buy), *price, *size))
        .collect();
    live_set.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    new_set == live_set
}
