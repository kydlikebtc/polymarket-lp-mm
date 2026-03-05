use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use polymarket_mm::config::AppConfig;
use polymarket_mm::data::state::{OrderSide, PnlTracker, PositionRecord, SharedState};

// ── PnlTracker Tests ──

#[test]
fn test_pnl_tracker_buy_then_sell_profit() {
    let mut tracker = PnlTracker::new();

    // Buy 100 shares at 0.40
    tracker.record_fill("m1", OrderSide::Buy, dec!(0.40), dec!(100));
    assert_eq!(tracker.realized_pnl, Decimal::ZERO); // No PnL until sell
    assert_eq!(tracker.fill_count, 1);

    // Sell 50 shares at 0.60 → PnL = (0.60 - 0.40) * 50 = 10.0
    tracker.record_fill("m1", OrderSide::Sell, dec!(0.60), dec!(50));
    assert_eq!(tracker.realized_pnl, dec!(10.0));
    assert_eq!(tracker.fill_count, 2);
}

#[test]
fn test_pnl_tracker_buy_then_sell_loss() {
    let mut tracker = PnlTracker::new();

    // Buy 100 at 0.60, sell 100 at 0.40 → PnL = (0.40 - 0.60) * 100 = -20
    tracker.record_fill("m1", OrderSide::Buy, dec!(0.60), dec!(100));
    tracker.record_fill("m1", OrderSide::Sell, dec!(0.40), dec!(100));
    assert_eq!(tracker.realized_pnl, dec!(-20));
}

#[test]
fn test_pnl_tracker_weighted_avg_cost() {
    let mut tracker = PnlTracker::new();

    // Buy 100 at 0.40, then 100 at 0.60 → avg = 0.50
    tracker.record_fill("m1", OrderSide::Buy, dec!(0.40), dec!(100));
    tracker.record_fill("m1", OrderSide::Buy, dec!(0.60), dec!(100));

    // Sell 100 at 0.55 → PnL = (0.55 - 0.50) * 100 = 5.0
    tracker.record_fill("m1", OrderSide::Sell, dec!(0.55), dec!(100));
    assert_eq!(tracker.realized_pnl, dec!(5.0));
}

#[test]
fn test_pnl_tracker_multi_market_independent() {
    let mut tracker = PnlTracker::new();

    // Market 1: buy at 0.40, sell at 0.50 → PnL = 10
    tracker.record_fill("m1", OrderSide::Buy, dec!(0.40), dec!(100));
    tracker.record_fill("m1", OrderSide::Sell, dec!(0.50), dec!(100));

    // Market 2: buy at 0.80, sell at 0.70 → PnL = -10
    tracker.record_fill("m2", OrderSide::Buy, dec!(0.80), dec!(100));
    tracker.record_fill("m2", OrderSide::Sell, dec!(0.70), dec!(100));

    // Total PnL: 10 + (-10) = 0
    assert_eq!(tracker.realized_pnl, Decimal::ZERO);
}

#[test]
fn test_pnl_tracker_zero_size_ignored() {
    let mut tracker = PnlTracker::new();
    tracker.record_fill("m1", OrderSide::Buy, dec!(0.50), Decimal::ZERO);
    assert_eq!(tracker.fill_count, 0); // Zero-size fills should be ignored
}

#[test]
fn test_pnl_tracker_sell_with_no_basis() {
    let mut tracker = PnlTracker::new();
    // Sell without any prior buy → avg_cost=0, PnL = (0.50 - 0) * 100 = 50
    tracker.record_fill("m1", OrderSide::Sell, dec!(0.50), dec!(100));
    assert_eq!(tracker.realized_pnl, dec!(50));
}

#[test]
fn test_pnl_tracker_complete_sell_resets_basis() {
    let mut tracker = PnlTracker::new();

    // Buy 100 at 0.40, sell all 100 at 0.60
    tracker.record_fill("m1", OrderSide::Buy, dec!(0.40), dec!(100));
    tracker.record_fill("m1", OrderSide::Sell, dec!(0.60), dec!(100));
    assert_eq!(tracker.realized_pnl, dec!(20));

    // Buy another batch at 0.30, sell at 0.50 → PnL from this batch alone
    tracker.record_fill("m1", OrderSide::Buy, dec!(0.30), dec!(50));
    tracker.record_fill("m1", OrderSide::Sell, dec!(0.50), dec!(50));
    // Additional PnL = (0.50 - 0.30) * 50 = 10
    assert_eq!(tracker.realized_pnl, dec!(30)); // 20 + 10
}

// ── PositionRecord Tests ──

#[test]
fn test_iir_zero_allocated_capital() {
    let pos = PositionRecord {
        market_id: "m1".to_string(),
        yes_shares: dec!(100),
        no_shares: Decimal::ZERO,
        yes_value: dec!(50),
        no_value: Decimal::ZERO,
        allocated_capital: Decimal::ZERO,
        updated_at: chrono::Utc::now(),
    };
    assert_eq!(pos.iir(), Decimal::ZERO);
}

#[test]
fn test_iir_clamped_to_one() {
    let pos = PositionRecord {
        market_id: "m1".to_string(),
        yes_shares: dec!(1000),
        no_shares: Decimal::ZERO,
        yes_value: dec!(900),
        no_value: Decimal::ZERO,
        allocated_capital: dec!(100),
        updated_at: chrono::Utc::now(),
    };
    // Raw IIR = 900/100 = 9.0, should be clamped to 1.0
    assert_eq!(pos.iir(), Decimal::ONE);
}

#[test]
fn test_iir_negative_clamped() {
    let pos = PositionRecord {
        market_id: "m1".to_string(),
        yes_shares: Decimal::ZERO,
        no_shares: dec!(1000),
        yes_value: Decimal::ZERO,
        no_value: dec!(900),
        allocated_capital: dec!(100),
        updated_at: chrono::Utc::now(),
    };
    // Raw IIR = (0 - 900)/100 = -9.0, clamped to -1.0
    assert_eq!(pos.iir(), -Decimal::ONE);
}

#[test]
fn test_iir_balanced_position() {
    let pos = PositionRecord {
        market_id: "m1".to_string(),
        yes_shares: dec!(100),
        no_shares: dec!(100),
        yes_value: dec!(50),
        no_value: dec!(50),
        allocated_capital: dec!(500),
        updated_at: chrono::Utc::now(),
    };
    // IIR = (50 - 50) / 500 = 0.0
    assert_eq!(pos.iir(), Decimal::ZERO);
}

#[test]
fn test_mergeable_amount() {
    let pos = PositionRecord {
        market_id: "m1".to_string(),
        yes_shares: dec!(150),
        no_shares: dec!(100),
        yes_value: Decimal::ZERO,
        no_value: Decimal::ZERO,
        allocated_capital: dec!(500),
        updated_at: chrono::Utc::now(),
    };
    assert_eq!(pos.mergeable_amount(), dec!(100)); // min(150, 100)
}

// ── PnL Date Rollover Tests ──

#[test]
fn test_pnl_date_rollover_resets_pnl_but_keeps_cost_basis() {
    let mut tracker = PnlTracker::new();

    // Build up some PnL: buy 100 at 0.40, sell 50 at 0.60 → PnL = 10
    tracker.record_fill("m1", OrderSide::Buy, dec!(0.40), dec!(100));
    tracker.record_fill("m1", OrderSide::Sell, dec!(0.60), dec!(50));
    assert_eq!(tracker.realized_pnl, dec!(10));
    assert_eq!(tracker.fill_count, 2);

    // Simulate day rollover by setting date to yesterday
    tracker.date = (chrono::Utc::now() - chrono::TimeDelta::days(1)).date_naive();

    // Next fill triggers rollover: PnL resets but cost basis is preserved
    // Remaining 50 shares at avg_cost 0.40, sell 50 at 0.50 → new day PnL = 5
    tracker.record_fill("m1", OrderSide::Sell, dec!(0.50), dec!(50));
    assert_eq!(tracker.realized_pnl, dec!(5), "PnL should be reset to new-day value only");
    assert_eq!(tracker.fill_count, 1, "Fill count should reset on day rollover");
}

// ── price_change_5min Tests ──

#[test]
fn test_price_change_5min_max_min_range() {
    // Need a config with at least one market to create SharedState
    let config = minimal_config();
    let state = SharedState::new(&config);

    // Record multiple prices simulating a spike-then-revert pattern
    // All timestamps are "now", so they're within the 5-min window
    state.record_price("m1", dec!(0.50));
    state.record_price("m1", dec!(0.55));
    state.record_price("m1", dec!(0.60)); // max
    state.record_price("m1", dec!(0.52));
    state.record_price("m1", dec!(0.50)); // min

    // Max-min range = 0.60 - 0.50 = 0.10
    // First-last would be 0.50 - 0.50 = 0.00 (wrong!)
    let change = state.price_change_5min("m1");
    assert_eq!(change, dec!(0.10), "Should use max-min range, not first-last");
}

#[test]
fn test_price_change_5min_single_point_returns_zero() {
    let config = minimal_config();
    let state = SharedState::new(&config);

    state.record_price("m1", dec!(0.50));

    // Only 1 data point → should return 0 (need >= 2 for meaningful range)
    let change = state.price_change_5min("m1");
    assert_eq!(change, Decimal::ZERO, "Single price point should return zero");
}

#[test]
fn test_price_change_5min_unknown_market_returns_zero() {
    let config = minimal_config();
    let state = SharedState::new(&config);

    let change = state.price_change_5min("nonexistent-market");
    assert_eq!(change, Decimal::ZERO, "Unknown market should return zero");
}

// ── Helper ──

fn minimal_config() -> AppConfig {
    use polymarket_mm::config::*;

    AppConfig {
        markets: vec![MarketConfig {
            market_id: "m1".to_string(),
            token_id: "t1".to_string(),
            name: "Test".to_string(),
            max_incentive_spread: dec!(0.03),
            min_size: dec!(5),
        }],
        capital: CapitalConfig {
            total_capital: dec!(1000),
            max_per_market_fraction: dec!(0.5),
        },
        pricing: PricingConfig {
            layers: vec![LayerConfig {
                distance: dec!(0.01),
                capital_fraction: dec!(1.0),
            }],
            base_half_spread: dec!(0.005),
            vaf_min: dec!(0.8),
            vaf_max: dec!(5.0),
            skew_factor: dec!(0.02),
            requote_threshold: dec!(0.005),
            requote_interval_secs: 30,
            baseline_volatility: dec!(0.025),
        },
        position: PositionConfig {
            iir_medium_threshold: dec!(0.5),
            iir_extreme_threshold: dec!(0.75),
            min_merge_size: dec!(100),
            merge_cooldown_secs: 3600,
        },
        risk: RiskConfig {
            l2_iir_threshold: dec!(0.5),
            l2_price_change_5min: dec!(0.05),
            l2_daily_loss_pct: dec!(0.03),
            l2_ws_disconnect_secs: 30,
            l3_iir_threshold: dec!(0.75),
            l3_price_change_5min: dec!(0.10),
            l3_daily_loss_pct: dec!(0.08),
            l3_ghost_fill_count: 3,
            l3_ghost_fill_window_secs: 1800,
            l2_timeout_to_l3_secs: 7200,
            l2_recovery_iir: dec!(0.4),
            l2_recovery_price_change: dec!(0.03),
            l2_recovery_hold_secs: 300,
            l2_size_multiplier: dec!(0.5),
            l2_spread_multiplier: dec!(1.5),
        },
        execution: ExecutionConfig {
            batch_size: 15,
            max_retries: 3,
            base_retry_delay_ms: 500,
            max_retry_delay_ms: 5000,
            cancel_confirm_timeout_ms: 5000,
        },
        api: ApiConfig {
            clob_base_url: "https://clob.polymarket.com".to_string(),
            gamma_base_url: "https://gamma-api.polymarket.com".to_string(),
            ws_market_url: "wss://ws-subscriptions-clob.polymarket.com/ws/market".to_string(),
            ws_user_url: "wss://ws-subscriptions-clob.polymarket.com/ws/user".to_string(),
            polygon_rpc_url: "https://polygon-rpc.com".to_string(),
            ctf_contract: "0x0000000000000000000000000000000000000001".to_string(),
        },
    }
}
