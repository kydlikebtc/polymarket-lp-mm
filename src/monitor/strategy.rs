//! R6-9: Market strategy logic extracted from monitor/mod.rs for maintainability.
//! Contains per-market strategy execution and risk evaluation.

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::AppConfig;
use crate::data::SharedState;
use crate::data::gamma;
use crate::execution::OrderExecutor;
use crate::pricing::PricingEngine;
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
    config: &AppConfig,
    state: &SharedState,
    pricing_engine: &PricingEngine,
    executor: &mut OrderExecutor,
    risk_controller: &Arc<Mutex<RiskController>>,
    risk_level: RiskLevel,
    per_market_capital: Decimal,
    last_midpoints: &mut HashMap<String, Decimal>,
    last_times: &mut HashMap<String, DateTime<Utc>>,
) -> Result<()> {
    let Some(market_config) = config.markets.iter().find(|m| m.market_id == market_id) else {
        return Ok(());
    };

    let Some(ms) = state.market_states.get(market_id) else {
        return Ok(());
    };

    let current_midpoint = ms.midpoint;
    let now = Utc::now();

    // Check if we need to re-quote
    let should_requote = {
        let last_mid = last_midpoints.get(market_id).copied();
        let last_time = last_times.get(market_id).copied();

        let price_moved = last_mid
            .map(|lm| (current_midpoint - lm).abs() >= config.pricing.requote_threshold)
            .unwrap_or(true);

        let time_expired = last_time
            .map(|lt| {
                (now - lt).num_seconds() >= config.pricing.requote_interval_secs as i64
            })
            .unwrap_or(true);

        price_moved || time_expired
    };

    if !should_requote {
        return Ok(());
    }

    // Get current IIR
    let iir = state
        .positions
        .get(market_id)
        .map(|p| p.iir())
        .unwrap_or(Decimal::ZERO);

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
        return Ok(());
    }

    // Step 1: Cancel existing orders (lock acquired briefly inside method)
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
    let ws_gap = state.max_ws_disconnect_secs().await;
    if ws_gap > 10 {
        warn!(
            "WS stale for {ws_gap}s, skipping order submission for market={market_id} to avoid blind exposure"
        );
        return Ok(());
    }

    // Step 2: Generate new quotes
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
    );

    if orders.is_empty() {
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

    // Step 3: Submit new orders
    executor.submit_orders(state, orders).await?;

    // Update tracking
    last_midpoints.insert(market_id.to_string(), current_midpoint);
    last_times.insert(market_id.to_string(), now);

    Ok(())
}
