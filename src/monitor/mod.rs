use anyhow::Result;
use chrono::Utc;
use rust_decimal::Decimal;
use tokio::signal;
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};

use crate::config::AppConfig;
use crate::data::SharedState;
use crate::execution::OrderExecutor;
use crate::position::{PositionAction, PositionManager};
use crate::pricing::PricingEngine;
use crate::risk::{RiskController, RiskLevel};

/// Main orchestration loop: ties all modules together.
///
/// Lifecycle:
/// 1. Start WebSocket connections (market + user channels)
/// 2. Pull initial positions from API
/// 3. Enter main loop: check risk → compute prices → manage positions → execute
/// 4. Handle graceful shutdown on Ctrl+C
pub async fn run_orchestrator(
    config: AppConfig,
    state: SharedState,
    mut risk_controller: RiskController,
    mut executor: OrderExecutor,
    pricing_engine: PricingEngine,
    position_manager: PositionManager,
) -> Result<()> {
    risk_controller.set_total_capital(config.capital.total_capital);

    let per_market_capital = config.per_market_capital();
    info!("Per-market capital allocation: ${per_market_capital:.2}");

    // Spawn WebSocket tasks
    // TODO: Uncomment when WebSocket implementation is ready
    // let market_ids: Vec<String> = config.markets.iter().map(|m| m.market_id.clone()).collect();
    // tokio::spawn(ws::run_market_ws(state.clone(), config.api.ws_market_url.clone(), market_ids));
    // tokio::spawn(ws::run_user_ws(state.clone(), config.api.ws_user_url.clone(), api_key));

    // Main strategy loop interval
    let mut strategy_tick = interval(Duration::from_secs(10));
    // Position check interval
    let mut position_tick = interval(Duration::from_secs(60));
    // Metrics logging interval
    let mut metrics_tick = interval(Duration::from_secs(60));

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
                    &mut risk_controller,
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
                            if let Err(e) = executor.cancel_all_orders(&state, &mut risk_controller).await {
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
                                &mut risk_controller,
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
                            handle_position_action(&action, &mut executor, &mut risk_controller, &state).await;
                        }
                    }
                }
            }

            // Metrics logging (every 60 seconds)
            _ = metrics_tick.tick() => {
                log_metrics(&state, &config, &risk_controller);
            }

            // Graceful shutdown
            _ = signal::ctrl_c() => {
                warn!("Ctrl+C received, initiating graceful shutdown...");
                if let Err(e) = executor.cancel_all_orders(&state, &mut risk_controller).await {
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
    controller: &mut RiskController,
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

    controller.evaluate(&market_iirs, &price_changes, ws_disconnect)
}

/// Run strategy for a single market
async fn run_market_strategy(
    market_id: &str,
    config: &AppConfig,
    state: &SharedState,
    pricing_engine: &PricingEngine,
    executor: &mut OrderExecutor,
    risk_controller: &mut RiskController,
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
    let tf = pricing_engine.compute_tf(None); // TODO: pass actual settlement time

    // TF = 0 means stop market making
    if tf.is_zero() {
        info!("Market {market_id} approaching settlement, stopping");
        executor.cancel_market_orders(state, market_id, risk_controller).await?;
        return Ok(());
    }

    // Step 1: Cancel existing orders
    executor.cancel_market_orders(state, market_id, risk_controller).await?;

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
    risk_controller: &mut RiskController,
    state: &SharedState,
) {
    match action {
        PositionAction::TriggerMerge { market_id, amount } => {
            info!("Would merge {amount} pairs in market={market_id}");
            // TODO: Call CTF contract mergePositions via alloy
        }
        PositionAction::EscalateL2 { market_id, iir } => {
            warn!("Position escalation to L2 from PositionManager: market={market_id}, IIR={iir}");
            // Drive RiskController to L2 by feeding high IIR
            let iirs = vec![(market_id.clone(), *iir)];
            let prices = vec![(market_id.clone(), Decimal::ZERO)];
            risk_controller.evaluate(&iirs, &prices, 0);
        }
        PositionAction::EscalateL3 { market_id, iir } => {
            warn!("Position escalation to L3 from PositionManager: market={market_id}, IIR={iir}");
            // Drive RiskController to L3 by feeding extreme IIR
            let iirs = vec![(market_id.clone(), *iir)];
            let prices = vec![(market_id.clone(), Decimal::ZERO)];
            risk_controller.evaluate(&iirs, &prices, 0);
            if risk_controller.level() == RiskLevel::L3Emergency {
                if let Err(e) = executor.cancel_all_orders(state, risk_controller).await {
                    error!("Failed to cancel orders for L3 escalation: {e:#}");
                }
            }
        }
        _ => {} // Skew actions are handled by pricing engine
    }
}

/// Log current metrics
fn log_metrics(
    state: &SharedState,
    config: &AppConfig,
    risk_controller: &RiskController,
) {
    let active_orders = state
        .my_orders
        .iter()
        .filter(|o| matches!(o.status, crate::data::state::OrderStatus::Live))
        .count();

    info!(
        "METRICS | Risk={} | ActiveOrders={} | Markets={}",
        risk_controller.level(),
        active_orders,
        config.markets.len()
    );

    for market in &config.markets {
        if let Some(ms) = state.market_states.get(&market.market_id) {
            let iir = state
                .positions
                .get(&market.market_id)
                .map(|p| p.iir())
                .unwrap_or(Decimal::ZERO);
            info!(
                "  Market {} | mid={} | spread={} | IIR={iir}",
                market.name, ms.midpoint, ms.spread
            );
        }
    }
}
