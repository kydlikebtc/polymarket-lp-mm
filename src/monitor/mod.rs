mod strategy;

use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use rust_decimal::Decimal;
use tokio::signal;
use tokio::sync::Mutex;
use tokio::time::{Duration, interval};
use tracing::{debug, error, info, warn};

use crate::config::AppConfig;
use crate::data::SharedState;
use crate::data::gamma::{self, GammaClient};
use crate::data::ws;
use crate::execution::OrderExecutor;
use crate::position::{PositionAction, PositionManager};
use crate::pricing::PricingEngine;
use crate::risk::{RiskController, RiskLevel};

use self::strategy::{evaluate_risk, run_market_strategy};

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
    mut position_manager: PositionManager,
) -> Result<()> {
    {
        let mut rc = risk_controller.lock().await;
        rc.set_total_capital(config.capital.total_capital);
    }

    let per_market_capital = config.per_market_capital();
    debug!("Per-market capital allocation: ${per_market_capital:.2}");

    // Load initial positions: fetch YES token balances for each market.
    // R6-10: Seed PnL cost basis with current midpoint as rough estimate for existing positions.
    // Without this, the first sell fills would compute PnL against avg_cost=0, showing inflated profits.
    let mut has_existing_positions = false;
    for market in &config.markets {
        match executor.client().fetch_token_balance(&market.token_id).await {
            Ok(balance) => {
                if balance > Decimal::ZERO {
                    has_existing_positions = true;
                    if let Some(mut pos) = state.positions.get_mut(&market.market_id) {
                        pos.yes_shares = balance;
                        // Compute initial yes_value so IIR is non-zero before first position_tick
                        if let Some(ms) = state.market_states.get(&market.market_id) {
                            pos.yes_value = balance * ms.midpoint;
                        }
                        pos.updated_at = Utc::now();
                        info!(
                            "Loaded initial position: market={}, YES shares={}, value={}",
                            market.name, balance, pos.yes_value
                        );
                    }

                    // Seed cost basis with default midpoint (0.50) as approximation.
                    // This is imprecise but prevents wildly wrong PnL on first sells.
                    {
                        let mut pnl = state.daily_pnl.write().await;
                        pnl.record_fill(
                            &market.market_id,
                            crate::data::state::OrderSide::Buy,
                            Decimal::new(50, 2), // 0.50 default
                            balance,
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
    if has_existing_positions {
        warn!(
            "Existing positions loaded with estimated cost basis (default midpoint 0.50). \
             PnL tracking will be approximate until positions are fully recycled."
        );
        // Reset realized_pnl to zero since the seed fills aren't real trades
        {
            let mut pnl = state.daily_pnl.write().await;
            pnl.realized_pnl = Decimal::ZERO;
            pnl.fill_count = 0;
        }
    }

    // Load existing open orders into state
    match executor.client().fetch_open_orders().await {
        Ok(orders) => {
            for order in &orders {
                // Resolve token → market; skip unrecognized tokens
                let Some(market_id) = state.resolve_market_id(&order.asset_id.to_string()) else {
                    warn!(
                        "Skipping open order {} with unrecognized token_id {}",
                        order.id, order.asset_id
                    );
                    continue;
                };
                // Map SDK side; skip unknown variants
                let side = match order.side {
                    polymarket_client_sdk::clob::types::Side::Buy => {
                        crate::data::state::OrderSide::Buy
                    }
                    polymarket_client_sdk::clob::types::Side::Sell => {
                        crate::data::state::OrderSide::Sell
                    }
                    other => {
                        warn!(
                            "Skipping open order {} with unknown side {:?}",
                            order.id, other
                        );
                        continue;
                    }
                };
                state.my_orders.insert(
                    order.id.clone(),
                    crate::data::state::OrderRecord {
                        order_id: order.id.clone(),
                        market_id,
                        price: order.price,
                        // R7-SEC4: Clamp to zero in case SDK returns size_matched > original_size
                        size: (order.original_size - order.size_matched).max(Decimal::ZERO),
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

    // R7-CQ3: Save WS JoinHandles to detect fatal task exits and trigger L3.
    // WS tasks have internal reconnection loops, so they should never exit.
    // If they do, something is fundamentally wrong and we must stop trading.
    let token_ids: Vec<String> = config.markets.iter().map(|m| m.token_id.clone()).collect();
    let ws_state = state.clone();
    let market_ws_handle = tokio::spawn(async move {
        if let Err(e) = ws::run_market_ws(ws_state, token_ids).await {
            error!("Market WebSocket fatal error: {e:#}");
        }
    });

    let ws_state = state.clone();
    let ws_credentials = executor.credentials().clone();
    let ws_address = executor.address();
    let ws_rc = Arc::clone(&risk_controller);
    let user_ws_handle = tokio::spawn(async move {
        if let Err(e) = ws::run_user_ws(ws_state, ws_credentials, ws_address, ws_rc).await {
            error!("User WebSocket fatal error: {e:#}");
        }
    });
    tokio::pin!(market_ws_handle);
    tokio::pin!(user_ws_handle);

    // Main strategy loop interval
    let mut strategy_tick = interval(Duration::from_secs(10));
    // Position check interval
    let mut position_tick = interval(Duration::from_secs(60));
    // Metrics logging interval
    let mut metrics_tick = interval(Duration::from_secs(60));
    // Settlement time refresh interval (every 30 minutes)
    let mut settlement_tick = interval(Duration::from_secs(1800));
    // State cleanup interval (every 5 minutes)
    let mut cleanup_tick = interval(Duration::from_secs(300));

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
                // R5-12: Skip strategy until both WS connections have received at least one message.
                // Before WS connects, midpoints are default (0.50) and may be wildly wrong.
                if !state.both_ws_connected() {
                    debug!("Waiting for WS connections before starting strategy");
                    continue;
                }

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
                            if let Err(e) = executor.cancel_all_orders(&state, &risk_controller).await {
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
                                &mut position_manager,
                            ).await;
                        }
                    }
                }
            }

            // Metrics logging (every 60 seconds)
            // Read risk level first, then release lock before reading PnL
            // to prevent deadlock (WS task acquires daily_pnl write → risk_controller lock)
            _ = metrics_tick.tick() => {
                let risk_level = {
                    let rc = risk_controller.lock().await;
                    rc.level()
                };
                log_metrics(&state, &config, risk_level).await;
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

            // Periodic cleanup of stale state (every 5 minutes)
            _ = cleanup_tick.tick() => {
                prune_stale_state(&state, &risk_controller).await;
            }

            // R7-CQ3: WS task fatal exit detection — trigger L3 and stop bot.
            // WS tasks have internal reconnect loops, so exit means unrecoverable failure.
            result = &mut market_ws_handle => {
                error!("Market WS task exited unexpectedly: {result:?}");
                let mut rc = risk_controller.lock().await;
                rc.force_l3(crate::risk::RiskTrigger::WsDisconnect { duration_secs: 0 });
                drop(rc);
                executor.cancel_all_orders(&state, &risk_controller).await.ok();
                return Err(anyhow::anyhow!("Market WebSocket task crashed — entering L3 and stopping"));
            }
            result = &mut user_ws_handle => {
                error!("User WS task exited unexpectedly: {result:?}");
                let mut rc = risk_controller.lock().await;
                rc.force_l3(crate::risk::RiskTrigger::WsDisconnect { duration_secs: 0 });
                drop(rc);
                executor.cancel_all_orders(&state, &risk_controller).await.ok();
                return Err(anyhow::anyhow!("User WebSocket task crashed — entering L3 and stopping"));
            }

            // Graceful shutdown
            _ = signal::ctrl_c() => {
                warn!("Ctrl+C received, initiating graceful shutdown...");
                if let Err(e) = executor.cancel_all_orders(&state, &risk_controller).await {
                    error!("Failed to cancel orders during shutdown: {e:#}");
                }
                info!("All orders cancelled. Shutdown complete.");
                return Ok(());
            }
        }
    }
}

/// Handle a position action, bridging PositionManager decisions to RiskController
async fn handle_position_action(
    action: &PositionAction,
    executor: &mut OrderExecutor,
    risk_controller: &Arc<Mutex<RiskController>>,
    state: &SharedState,
    position_manager: &mut PositionManager,
) {
    match action {
        PositionAction::TriggerMerge { market_id, amount } => {
            info!("Would merge {amount} pairs in market={market_id}");
            // TODO: Call CTF contract mergePositions via alloy
            // R5-18: Record merge timestamp for cooldown enforcement
            position_manager.record_merge(market_id);
        }
        PositionAction::EscalateL2 { market_id, iir } => {
            warn!("Position escalation to L2 from PositionManager: market={market_id}, IIR={iir}");
            let mut rc = risk_controller.lock().await;
            rc.force_l2(crate::risk::RiskTrigger::IirExceeded {
                market_id: market_id.clone(),
                iir: *iir,
            });
        }
        PositionAction::EscalateL3 { market_id, iir } => {
            warn!("Position escalation to L3 from PositionManager: market={market_id}, IIR={iir}");
            let should_cancel = {
                let mut rc = risk_controller.lock().await;
                rc.force_l3(crate::risk::RiskTrigger::IirExceeded {
                    market_id: market_id.clone(),
                    iir: *iir,
                });
                rc.level() == RiskLevel::L3Emergency
            };
            if should_cancel {
                if let Err(e) = executor.cancel_all_orders(state, risk_controller).await {
                    error!("Failed to cancel orders for L3 escalation: {e:#}");
                }
            }
        }
    }
}

/// Prune stale entries from my_orders and cancel_requests to prevent unbounded growth.
/// R6-2: Also prune Live/Pending orders that haven't been updated in over 1 hour,
/// which are likely stale (exchange may have silently cancelled them).
async fn prune_stale_state(
    state: &SharedState,
    risk_controller: &Arc<Mutex<RiskController>>,
) {
    let terminal_cutoff = Utc::now() - chrono::TimeDelta::minutes(5);
    let stale_live_cutoff = Utc::now() - chrono::TimeDelta::hours(1);

    // Remove orders that have been Canceled or Matched for over 5 minutes,
    // or Live/Pending orders not updated in over 1 hour
    let stale_ids: Vec<String> = state
        .my_orders
        .iter()
        .filter(|entry| {
            let is_terminal_stale = matches!(
                entry.status,
                crate::data::state::OrderStatus::Canceled | crate::data::state::OrderStatus::Matched
            ) && entry.updated_at < terminal_cutoff;

            let is_live_stale = matches!(
                entry.status,
                crate::data::state::OrderStatus::Live | crate::data::state::OrderStatus::Pending
            ) && entry.updated_at < stale_live_cutoff;

            is_terminal_stale || is_live_stale
        })
        .map(|entry| entry.order_id.clone())
        .collect();

    if !stale_ids.is_empty() {
        for id in &stale_ids {
            state.my_orders.remove(id);
        }
        info!("Pruned {} stale orders from local state", stale_ids.len());
    }

    // Clean expired cancel requests from RiskController
    {
        let mut rc = risk_controller.lock().await;
        rc.prune_stale_cancels(terminal_cutoff);
    }
}

/// Log current metrics
async fn log_metrics(
    state: &SharedState,
    config: &AppConfig,
    risk_level: RiskLevel,
) {
    let active_orders = state
        .my_orders
        .iter()
        .filter(|o| matches!(o.status, crate::data::state::OrderStatus::Live))
        .count();

    let pnl = state.daily_pnl.read().await;

    info!(
        "METRICS | Risk={} | ActiveOrders={} | Markets={} | DailyPnL=${:.2} | Fills={}",
        risk_level,
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
