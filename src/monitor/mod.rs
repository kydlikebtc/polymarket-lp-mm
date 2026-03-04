use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use rust_decimal::Decimal;
use tokio::signal;
use tokio::sync::Mutex;
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};

use crate::config::AppConfig;
use crate::data::SharedState;
use crate::data::gamma::{self, GammaClient};
use crate::data::ws;
use crate::execution::OrderExecutor;
use crate::position::{PositionAction, PositionManager};
use crate::pricing::PricingEngine;
use crate::risk::{RiskController, RiskLevel};

/// Main orchestration loop: ties all modules together.
///
/// Lifecycle:
/// 1. Start WebSocket connections (market + user channels)
/// 2. Enter main loop: check risk → compute prices → manage positions → execute
/// 3. Handle graceful shutdown on Ctrl+C
pub async fn run_orchestrator(
    config: AppConfig,
    state: SharedState,
    risk_controller: Arc<Mutex<RiskController>>,
    mut executor: OrderExecutor,
    pricing_engine: PricingEngine,
    position_manager: PositionManager,
) -> Result<()> {
    {
        let mut rc = risk_controller.lock().await;
        rc.set_total_capital(config.capital.total_capital);
    }

    let per_market_capital = config.per_market_capital();
    info!("Per-market capital allocation: ${per_market_capital:.2}");

    // Load initial positions: fetch YES token balances for each market
    for market in &config.markets {
        match executor.client().fetch_token_balance(&market.token_id).await {
            Ok(balance) => {
                if balance > Decimal::ZERO {
                    if let Some(mut pos) = state.positions.get_mut(&market.market_id) {
                        pos.yes_shares = balance;
                        pos.updated_at = Utc::now();
                        info!(
                            "Loaded initial position: market={}, YES shares={}",
                            market.name, balance
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    "Failed to load position for market={}: {e:#}, starting with zero",
                    market.name
                );
            }
        }
    }

    // Load existing open orders into state
    match executor.client().fetch_open_orders().await {
        Ok(orders) => {
            for order in &orders {
                let market_id = state
                    .resolve_market_id(&order.asset_id.to_string())
                    .unwrap_or_default();
                let side = match order.side {
                    polymarket_client_sdk::clob::types::Side::Buy => {
                        crate::data::state::OrderSide::Buy
                    }
                    polymarket_client_sdk::clob::types::Side::Sell => {
                        crate::data::state::OrderSide::Sell
                    }
                    _ => crate::data::state::OrderSide::Buy,
                };
                state.my_orders.insert(
                    order.id.clone(),
                    crate::data::state::OrderRecord {
                        order_id: order.id.clone(),
                        market_id,
                        price: order.price,
                        size: order.original_size - order.size_matched,
                        side,
                        status: crate::data::state::OrderStatus::Live,
                        created_at: order.created_at,
                        updated_at: Utc::now(),
                    },
                );
            }
            info!("Loaded {} existing open orders from API", orders.len());
        }
        Err(e) => {
            warn!("Failed to load open orders: {e:#}, starting fresh");
        }
    }

    // Fetch settlement times from Gamma API
    let gamma_client = GammaClient::new(&config.api.gamma_base_url)?;
    let market_ids: Vec<String> = config.markets.iter().map(|m| m.market_id.clone()).collect();
    let settlement_map = gamma_client.fetch_all_end_dates(&market_ids).await;
    for (market_id, end_date) in &settlement_map {
        state.settlement_times.insert(market_id.clone(), *end_date);
        let hours = gamma::hours_until(end_date).unwrap_or(0.0);
        info!("Market {market_id}: settles at {end_date}, {hours:.1}h remaining");
    }

    // Spawn market data WebSocket
    let token_ids: Vec<String> = config.markets.iter().map(|m| m.token_id.clone()).collect();
    let ws_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = ws::run_market_ws(ws_state, token_ids).await {
            error!("Market WebSocket fatal error: {e:#}");
        }
    });

    // Spawn user events WebSocket (for ghost fill detection)
    let ws_state = state.clone();
    let ws_credentials = executor.credentials().clone();
    let ws_address = executor.address();
    let ws_rc = Arc::clone(&risk_controller);
    tokio::spawn(async move {
        if let Err(e) = ws::run_user_ws(ws_state, ws_credentials, ws_address, ws_rc).await {
            error!("User WebSocket fatal error: {e:#}");
        }
    });

    // Main strategy loop interval
    let mut strategy_tick = interval(Duration::from_secs(10));
    // Position check interval
    let mut position_tick = interval(Duration::from_secs(60));
    // Metrics logging interval
    let mut metrics_tick = interval(Duration::from_secs(60));
    // Settlement time refresh interval (every 30 minutes)
    let mut settlement_tick = interval(Duration::from_secs(1800));

    // Track last quote midpoints per market
    let mut last_quote_midpoints: std::collections::HashMap<String, Decimal> =
        std::collections::HashMap::new();
    let mut last_quote_times: std::collections::HashMap<String, chrono::DateTime<Utc>> =
        std::collections::HashMap::new();

    info!("Main loop started. Press Ctrl+C to stop gracefully.");

    loop {
        tokio::select! {
            // Strategy tick (every 10 seconds)
            _ = strategy_tick.tick() => {
                let risk_level = evaluate_risk(
                    &risk_controller,
                    &state,
                    &config,
                ).await;

                match risk_level {
                    RiskLevel::L3Emergency => {
                        // Emergency: cancel everything (only if not already cancelled)
                        let has_active = state.my_orders.iter().any(|o| {
                            matches!(o.status, crate::data::state::OrderStatus::Live | crate::data::state::OrderStatus::Pending)
                        });
                        if has_active {
                            let mut rc = risk_controller.lock().await;
                            if let Err(e) = executor.cancel_all_orders(&state, &mut rc).await {
                                error!("Failed to cancel all orders in L3: {e:#}");
                            }
                            warn!("L3 Emergency active. All orders cancelled. Waiting for manual recovery.");
                        }
                        continue;
                    }
                    _ => {
                        // L1 or L2: run strategy for each market
                        for market in &config.markets {
                            if let Err(e) = run_market_strategy(
                                &market.market_id,
                                &config,
                                &state,
                                &pricing_engine,
                                &mut executor,
                                &risk_controller,
                                risk_level,
                                per_market_capital,
                                &mut last_quote_midpoints,
                                &mut last_quote_times,
                            ).await {
                                error!("Strategy error for market={}: {e:#}", market.market_id);
                            }
                        }
                    }
                }
            }

            // Position management tick (every 60 seconds)
            _ = position_tick.tick() => {
                for market in &config.markets {
                    // Update position values with current midpoint
                    if let Some(ms) = state.market_states.get(&market.market_id) {
                        position_manager.update_position_values(
                            &state,
                            &market.market_id,
                            ms.midpoint,
                        );
                    }

                    // Evaluate position actions
                    if let Some(pos) = state.positions.get(&market.market_id) {
                        let actions = position_manager.evaluate(&pos);
                        for action in actions {
                            handle_position_action(
                                &action,
                                &mut executor,
                                &risk_controller,
                                &state,
                            ).await;
                        }
                    }
                }
            }

            // Metrics logging (every 60 seconds)
            _ = metrics_tick.tick() => {
                let rc = risk_controller.lock().await;
                log_metrics(&state, &config, &rc).await;
            }

            // Refresh settlement times from Gamma API (every 30 minutes)
            _ = settlement_tick.tick() => {
                let updated = gamma_client.fetch_all_end_dates(&market_ids).await;
                for (market_id, end_date) in &updated {
                    state.settlement_times.insert(market_id.clone(), *end_date);
                }
                if !updated.is_empty() {
                    info!("Refreshed settlement times for {} markets", updated.len());
                }
            }

            // Graceful shutdown
            _ = signal::ctrl_c() => {
                warn!("Ctrl+C received, initiating graceful shutdown...");
                let mut rc = risk_controller.lock().await;
                if let Err(e) = executor.cancel_all_orders(&state, &mut rc).await {
                    error!("Failed to cancel orders during shutdown: {e:#}");
                }
                info!("All orders cancelled. Shutdown complete.");
                return Ok(());
            }
        }
    }
}

/// Evaluate risk conditions and return current level
async fn evaluate_risk(
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

    let ws_disconnect = state.ws_disconnect_secs().await;

    // Sync daily PnL to risk controller before evaluation
    let daily_pnl = {
        let pnl = state.daily_pnl.read().await;
        pnl.realized_pnl
    };

    let mut rc = controller.lock().await;
    rc.update_pnl(daily_pnl);
    rc.evaluate(&market_iirs, &price_changes, ws_disconnect)
}

/// Run strategy for a single market
async fn run_market_strategy(
    market_id: &str,
    config: &AppConfig,
    state: &SharedState,
    pricing_engine: &PricingEngine,
    executor: &mut OrderExecutor,
    risk_controller: &Arc<Mutex<RiskController>>,
    risk_level: RiskLevel,
    per_market_capital: Decimal,
    last_midpoints: &mut std::collections::HashMap<String, Decimal>,
    last_times: &mut std::collections::HashMap<String, chrono::DateTime<Utc>>,
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
    let hours_to_settlement = state
        .settlement_times
        .get(market_id)
        .and_then(|end_date| gamma::hours_until(&end_date));
    let tf = pricing_engine.compute_tf(hours_to_settlement);

    // TF = 0 means stop market making
    if tf.is_zero() {
        info!("Market {market_id} approaching settlement, stopping");
        let mut rc = risk_controller.lock().await;
        executor.cancel_market_orders(state, market_id, &mut rc).await?;
        return Ok(());
    }

    // Step 1: Cancel existing orders
    {
        let mut rc = risk_controller.lock().await;
        executor.cancel_market_orders(state, market_id, &mut rc).await?;
    }

    // Step 2: Generate new quotes
    let orders = pricing_engine.generate_quotes(
        market_config,
        current_midpoint,
        iir,
        vaf,
        tf,
        per_market_capital,
        risk_level,
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

/// Handle a position action, bridging PositionManager decisions to RiskController
async fn handle_position_action(
    action: &PositionAction,
    executor: &mut OrderExecutor,
    risk_controller: &Arc<Mutex<RiskController>>,
    state: &SharedState,
) {
    match action {
        PositionAction::TriggerMerge { market_id, amount } => {
            info!("Would merge {amount} pairs in market={market_id}");
            // TODO: Call CTF contract mergePositions via alloy
        }
        PositionAction::EscalateL2 { market_id, iir } => {
            warn!("Position escalation to L2 from PositionManager: market={market_id}, IIR={iir}");
            let mut rc = risk_controller.lock().await;
            let iirs = vec![(market_id.clone(), *iir)];
            let prices = vec![(market_id.clone(), Decimal::ZERO)];
            rc.evaluate(&iirs, &prices, 0);
        }
        PositionAction::EscalateL3 { market_id, iir } => {
            warn!("Position escalation to L3 from PositionManager: market={market_id}, IIR={iir}");
            let mut rc = risk_controller.lock().await;
            let iirs = vec![(market_id.clone(), *iir)];
            let prices = vec![(market_id.clone(), Decimal::ZERO)];
            rc.evaluate(&iirs, &prices, 0);
            if rc.level() == RiskLevel::L3Emergency {
                if let Err(e) = executor.cancel_all_orders(state, &mut rc).await {
                    error!("Failed to cancel orders for L3 escalation: {e:#}");
                }
            }
        }
        _ => {} // Skew actions are handled by pricing engine
    }
}

/// Log current metrics
async fn log_metrics(
    state: &SharedState,
    config: &AppConfig,
    risk_controller: &RiskController,
) {
    let active_orders = state
        .my_orders
        .iter()
        .filter(|o| matches!(o.status, crate::data::state::OrderStatus::Live))
        .count();

    let pnl = state.daily_pnl.read().await;

    info!(
        "METRICS | Risk={} | ActiveOrders={} | Markets={} | DailyPnL=${:.2} | Fills={}",
        risk_controller.level(),
        active_orders,
        config.markets.len(),
        pnl.realized_pnl,
        pnl.fill_count,
    );

    for market in &config.markets {
        if let Some(ms) = state.market_states.get(&market.market_id) {
            let iir = state
                .positions
                .get(&market.market_id)
                .map(|p| p.iir())
                .unwrap_or(Decimal::ZERO);
            let shares = state
                .positions
                .get(&market.market_id)
                .map(|p| (p.yes_shares, p.no_shares))
                .unwrap_or((Decimal::ZERO, Decimal::ZERO));
            info!(
                "  Market {} | mid={} | spread={} | IIR={iir} | YES={} NO={}",
                market.name, ms.midpoint, ms.spread, shares.0, shares.1,
            );
        }
    }
}
