use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::AppConfig;
use crate::data::gamma;
use crate::data::state::{OrderSide, OrderStatus};
use crate::data::SharedState;
use crate::pricing::PricingEngine;
use crate::risk::{RiskController, RiskLevel};
use crate::strategy::SharedRegistry;
use crate::tui::app::SearchResultItem;

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
    // ── Account & Capital ──
    pub usdc_balance: Decimal,
    pub total_capital: Decimal,
    pub deployed_capital: Decimal,
    // ── Execution stats ──
    pub orders_placed_total: u64,
    pub orders_cancelled_total: u64,
    /// Search results from Gamma API (populated asynchronously)
    pub search_results: Option<Vec<SearchResultItem>>,
    /// Available strategy profile names (from StrategyRegistry)
    pub profile_names: Vec<String>,
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
    /// LP reward: max incentive spread for this market (from config)
    pub max_incentive_spread: Decimal,
    /// LP reward: minimum order size for Q-Score qualification
    pub min_size: Decimal,
    /// LP reward: estimated Q-Score of current live orders
    pub estimated_q: Decimal,
    /// LP reward: whether current live orders qualify (dual-sided, in-spread, min_size)
    pub reward_qualified: bool,
    // ── Pricing factors ──
    pub vaf: Decimal,
    pub tf: Decimal,
    pub skew: Decimal,
    pub hours_to_settlement: Option<f64>,
    // ── Strategy management ──
    pub enabled: bool,
    pub profile_name: String,
    pub capital_allocation: Decimal,
}

#[derive(Debug, Clone)]
pub struct OrderSnapshot {
    pub order_id: String,
    pub market_id: String,
    pub market_name: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub status: String,
    pub age_secs: i64,
    /// LP reward: distance from midpoint (for this order's market)
    pub distance_from_mid: Decimal,
    /// LP reward: max incentive spread for this order's market
    pub max_spread: Decimal,
    /// LP reward: per-order Q contribution = ((max_spread - distance) / max_spread)² × size
    pub q_contribution: Decimal,
    /// LP reward: does this order qualify? (within spread + meets min_size)
    pub reward_eligible: bool,
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
    pricing_engine: &PricingEngine,
    strategy_registry: &SharedRegistry,
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

    // Account & execution stats
    let usdc_balance = *state.usdc_balance.read().await;
    let total_capital = config.capital.total_capital;
    let orders_placed_total = state.orders_placed_count.load(std::sync::atomic::Ordering::Relaxed);
    let orders_cancelled_total = state.orders_cancelled_count.load(std::sync::atomic::Ordering::Relaxed);

    // WS status
    let market_ws_connected = state.market_ws_connected.load(std::sync::atomic::Ordering::Acquire);
    let user_ws_connected = state.user_ws_connected.load(std::sync::atomic::Ordering::Acquire);
    let ws_disconnect_secs = state.max_ws_disconnect_secs().await;

    // Strategy registry info (single lock acquisition, then release)
    let (strategy_info, profile_names, dynamic_instances): (
        std::collections::HashMap<String, (bool, String, Decimal)>,
        Vec<String>,
        Vec<crate::strategy::StrategyInstance>,
    ) = {
        let registry = strategy_registry.read().await;
        let info = registry
            .all_instances()
            .iter()
            .map(|i| {
                (
                    i.market.market_id.clone(),
                    (i.enabled, i.profile_name.clone(), i.capital_allocation),
                )
            })
            .collect();
        let names = registry.profile_names();
        // Collect instances for markets NOT in config (dynamically added)
        let config_market_ids: std::collections::HashSet<&str> =
            config.markets.iter().map(|m| m.market_id.as_str()).collect();
        let dynamic: Vec<_> = registry
            .all_instances()
            .iter()
            .filter(|i| !config_market_ids.contains(i.market.market_id.as_str()))
            .cloned()
            .collect();
        (info, names, dynamic)
    };

    // Market snapshots
    let mut markets = Vec::with_capacity(config.markets.len());
    let mut deployed_capital = Decimal::ZERO;
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

        // LP reward: compute Q-Score of live orders for this market
        let max_incentive_spread = market_cfg.max_incentive_spread;
        let min_size = market_cfg.min_size;
        let live_orders_for_market: Vec<_> = state
            .my_orders
            .iter()
            .filter(|o| o.market_id == *mid && matches!(o.status, OrderStatus::Live))
            .map(|o| (o.side, o.price, o.size))
            .collect();

        let (estimated_q, reward_qualified) = compute_market_reward(
            &live_orders_for_market,
            midpoint,
            max_incentive_spread,
            min_size,
        );

        // Pricing factors for this market
        let vaf = pricing_engine.compute_vaf(state, mid);
        let hours_to_settlement = state
            .settlement_times
            .get(mid)
            .map(|end_date| gamma::hours_until(&end_date).unwrap_or(0.0));
        let tf = pricing_engine.compute_tf(hours_to_settlement);
        let skew = -iir * config.pricing.skew_factor;
        let skew = skew.max(dec!(-0.03)).min(dec!(0.03));

        // Deployed capital: sum of live order values (price × size) for this market
        let market_deployed: Decimal = state
            .my_orders
            .iter()
            .filter(|o| o.market_id == *mid && matches!(o.status, OrderStatus::Live))
            .map(|o| o.price * o.size)
            .sum();
        deployed_capital += market_deployed;

        // Strategy enabled/profile/capital from registry
        let (enabled, profile_name, capital_allocation) = strategy_info
            .get(mid)
            .cloned()
            .unwrap_or((true, "default".to_string(), Decimal::ZERO));

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
            max_incentive_spread,
            min_size,
            estimated_q,
            reward_qualified,
            vaf,
            tf,
            skew,
            hours_to_settlement,
            enabled,
            profile_name,
            capital_allocation,
        });
    }

    // FIX-5: Include dynamically added markets (not in config.markets)
    for dyn_instance in &dynamic_instances {
        let mid = &dyn_instance.market.market_id;

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

        let max_incentive_spread = dyn_instance.market.max_incentive_spread;
        let min_size = dyn_instance.market.min_size;
        let live_orders_for_market: Vec<_> = state
            .my_orders
            .iter()
            .filter(|o| o.market_id == *mid && matches!(o.status, OrderStatus::Live))
            .map(|o| (o.side, o.price, o.size))
            .collect();

        let (estimated_q, reward_qualified) = compute_market_reward(
            &live_orders_for_market, midpoint, max_incentive_spread, min_size,
        );

        let vaf = pricing_engine.compute_vaf(state, mid);
        let hours_to_settlement = state
            .settlement_times
            .get(mid)
            .map(|end_date| gamma::hours_until(&end_date).unwrap_or(0.0));
        let tf = pricing_engine.compute_tf(hours_to_settlement);
        let skew = -iir * config.pricing.skew_factor;
        let skew = skew.max(dec!(-0.03)).min(dec!(0.03));

        let market_deployed: Decimal = state
            .my_orders
            .iter()
            .filter(|o| o.market_id == *mid && matches!(o.status, OrderStatus::Live))
            .map(|o| o.price * o.size)
            .sum();
        deployed_capital += market_deployed;

        let (enabled, profile_name, capital_allocation) = strategy_info
            .get(mid)
            .cloned()
            .unwrap_or((dyn_instance.enabled, dyn_instance.profile_name.clone(), dyn_instance.capital_allocation));

        markets.push(MarketSnapshot {
            market_id: mid.clone(),
            name: dyn_instance.market.name.clone(),
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
            max_incentive_spread,
            min_size,
            estimated_q,
            reward_qualified,
            vaf,
            tf,
            skew,
            hours_to_settlement,
            enabled,
            profile_name,
            capital_allocation,
        });
    }

    // Build market lookup for per-order reward calculation (includes dynamic markets)
    let mut market_lookup: std::collections::HashMap<&str, (Decimal, Decimal, Decimal, &str)> = config
        .markets
        .iter()
        .map(|m| {
            let mid = state
                .market_states
                .get(&m.market_id)
                .map(|ms| ms.midpoint)
                .unwrap_or(Decimal::ZERO);
            (m.market_id.as_str(), (mid, m.max_incentive_spread, m.min_size, m.name.as_str()))
        })
        .collect();
    for dyn_inst in &dynamic_instances {
        let mid = state
            .market_states
            .get(&dyn_inst.market.market_id)
            .map(|ms| ms.midpoint)
            .unwrap_or(Decimal::ZERO);
        market_lookup.entry(dyn_inst.market.market_id.as_str()).or_insert(
            (mid, dyn_inst.market.max_incentive_spread, dyn_inst.market.min_size, dyn_inst.market.name.as_str())
        );
    }

    // Order snapshots with LP reward fields
    let mut orders: Vec<OrderSnapshot> = state
        .my_orders
        .iter()
        .map(|entry| {
            let o = entry.value();
            let (midpoint, max_spread, min_size, market_name) = market_lookup
                .get(o.market_id.as_str())
                .copied()
                .unwrap_or((Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, ""));

            let distance = (o.price - midpoint).abs();
            let in_spread = distance < max_spread && !max_spread.is_zero();
            let meets_size = o.size >= min_size;
            let reward_eligible = in_spread && meets_size;

            let q_contribution = if in_spread && !max_spread.is_zero() {
                let ratio = (max_spread - distance) / max_spread;
                ratio * ratio * o.size
            } else {
                Decimal::ZERO
            };

            OrderSnapshot {
                order_id: o.order_id.clone(),
                market_id: o.market_id.clone(),
                market_name: market_name.to_string(),
                side: o.side.to_string(),
                price: o.price,
                size: o.size,
                status: format!("{:?}", o.status),
                age_secs: (now - o.created_at).num_seconds(),
                distance_from_mid: distance,
                max_spread,
                q_contribution,
                reward_eligible,
            }
        })
        .collect();
    // Sort: Live first, then by newest first (ascending age_secs = most recent on top)
    orders.sort_by(|a, b| {
        let status_ord = |s: &str| -> u8 {
            match s {
                "Live" => 0,
                "Pending" => 1,
                "Matched" => 2,
                "Canceled" => 3,
                _ => 4,
            }
        };
        status_ord(&a.status)
            .cmp(&status_ord(&b.status))
            .then(a.age_secs.cmp(&b.age_secs))
    });

    // Price history snapshots (last 60 minutes) — includes dynamic markets
    let base_ts = now.timestamp() as f64;
    let mut price_histories = Vec::new();
    let mut seen_markets = std::collections::HashSet::new();
    for market_cfg in &config.markets {
        let mid = &market_cfg.market_id;
        seen_markets.insert(mid.clone());
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
    for dyn_inst in &dynamic_instances {
        let mid = &dyn_inst.market.market_id;
        if seen_markets.contains(mid) {
            continue;
        }
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
        usdc_balance,
        total_capital,
        deployed_capital,
        orders_placed_total,
        orders_cancelled_total,
        search_results: None,
        profile_names,
    }
}

/// Compute LP reward Q-Score and qualification for a market's live orders.
/// Returns (estimated_q, reward_qualified).
/// reward_qualified = true when both BID and ASK exist within spread with min_size.
fn compute_market_reward(
    live_orders: &[(OrderSide, Decimal, Decimal)], // (side, price, size)
    midpoint: Decimal,
    max_spread: Decimal,
    min_size: Decimal,
) -> (Decimal, bool) {
    if max_spread.is_zero() || live_orders.is_empty() {
        return (Decimal::ZERO, false);
    }

    let mut total_q = Decimal::ZERO;
    let mut has_qualifying_bid = false;
    let mut has_qualifying_ask = false;

    for (side, price, size) in live_orders {
        let distance = (*price - midpoint).abs();
        if distance >= max_spread {
            continue;
        }

        let ratio = (max_spread - distance) / max_spread;
        let q = ratio * ratio * *size;
        total_q += q;

        if *size >= min_size {
            match side {
                OrderSide::Buy => has_qualifying_bid = true,
                OrderSide::Sell => has_qualifying_ask = true,
            }
        }
    }

    // Single-side penalty: ÷3 (matches Polymarket's reward formula)
    let dual_sided = has_qualifying_bid && has_qualifying_ask;
    if !dual_sided {
        total_q /= dec!(3);
    }

    (total_q.round_dp(1), dual_sided)
}
