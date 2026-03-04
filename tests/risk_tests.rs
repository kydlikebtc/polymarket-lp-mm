use rust_decimal_macros::dec;

use polymarket_mm::config::RiskConfig;
use polymarket_mm::risk::{RiskController, RiskLevel};

fn test_risk_config() -> RiskConfig {
    RiskConfig {
        l2_iir_threshold: dec!(0.5),
        l2_price_change_5min: dec!(0.10),
        l2_daily_loss_pct: dec!(0.03),
        l2_ws_disconnect_secs: 30,
        l3_iir_threshold: dec!(0.75),
        l3_price_change_5min: dec!(0.20),
        l3_daily_loss_pct: dec!(0.08),
        l3_ghost_fill_count: 3,
        l3_ghost_fill_window_secs: 1800,
        l2_timeout_to_l3_secs: 7200,
        l2_recovery_iir: dec!(0.4),
        l2_recovery_price_change: dec!(0.05),
        l2_recovery_hold_secs: 0, // Instant recovery for testing
        l2_size_multiplier: dec!(0.5),
        l2_spread_multiplier: dec!(1.5),
    }
}

#[test]
fn test_starts_at_l1() {
    let controller = RiskController::new(&test_risk_config());
    assert_eq!(controller.level(), RiskLevel::L1Normal);
}

#[test]
fn test_iir_triggers_l2() {
    let mut controller = RiskController::new(&test_risk_config());
    controller.set_total_capital(dec!(10000));

    let iirs = vec![("market-1".to_string(), dec!(0.55))];
    let prices = vec![("market-1".to_string(), dec!(0.02))];

    let level = controller.evaluate(&iirs, &prices, 0);

    assert_eq!(level, RiskLevel::L2Warning, "IIR=0.55 should trigger L2");
}

#[test]
fn test_extreme_iir_triggers_l3() {
    let mut controller = RiskController::new(&test_risk_config());
    controller.set_total_capital(dec!(10000));

    let iirs = vec![("market-1".to_string(), dec!(0.80))];
    let prices = vec![("market-1".to_string(), dec!(0.02))];

    let level = controller.evaluate(&iirs, &prices, 0);

    assert_eq!(level, RiskLevel::L3Emergency, "IIR=0.80 should trigger L3");
}

#[test]
fn test_price_jump_triggers_l2() {
    let mut controller = RiskController::new(&test_risk_config());
    controller.set_total_capital(dec!(10000));

    let iirs = vec![("market-1".to_string(), dec!(0.1))];
    let prices = vec![("market-1".to_string(), dec!(0.12))]; // 12% jump

    let level = controller.evaluate(&iirs, &prices, 0);

    assert_eq!(level, RiskLevel::L2Warning, "12% price jump should trigger L2");
}

#[test]
fn test_extreme_price_jump_triggers_l3() {
    let mut controller = RiskController::new(&test_risk_config());
    controller.set_total_capital(dec!(10000));

    let iirs = vec![("market-1".to_string(), dec!(0.1))];
    let prices = vec![("market-1".to_string(), dec!(0.25))]; // 25% jump

    let level = controller.evaluate(&iirs, &prices, 0);

    assert_eq!(level, RiskLevel::L3Emergency, "25% price jump should trigger L3");
}

#[test]
fn test_daily_loss_triggers_l2() {
    let mut controller = RiskController::new(&test_risk_config());
    controller.set_total_capital(dec!(10000));
    controller.update_pnl(dec!(-350)); // -3.5% loss

    let iirs = vec![("market-1".to_string(), dec!(0.1))];
    let prices = vec![("market-1".to_string(), dec!(0.01))];

    let level = controller.evaluate(&iirs, &prices, 0);

    assert_eq!(level, RiskLevel::L2Warning, "-3.5% daily loss should trigger L2");
}

#[test]
fn test_ws_disconnect_triggers_l2() {
    let mut controller = RiskController::new(&test_risk_config());
    controller.set_total_capital(dec!(10000));

    let iirs = vec![("market-1".to_string(), dec!(0.1))];
    let prices = vec![("market-1".to_string(), dec!(0.01))];

    let level = controller.evaluate(&iirs, &prices, 35); // 35s disconnect

    assert_eq!(level, RiskLevel::L2Warning, "35s WS disconnect should trigger L2");
}

#[test]
fn test_l2_recovers_to_l1() {
    let mut config = test_risk_config();
    config.l2_recovery_hold_secs = 0; // Instant recovery for testing

    let mut controller = RiskController::new(&config);
    controller.set_total_capital(dec!(10000));

    // Trigger L2
    let iirs_high = vec![("market-1".to_string(), dec!(0.55))];
    let prices = vec![("market-1".to_string(), dec!(0.02))];
    controller.evaluate(&iirs_high, &prices, 0);
    assert_eq!(controller.level(), RiskLevel::L2Warning);

    // Conditions recover
    let iirs_low = vec![("market-1".to_string(), dec!(0.2))];
    let prices_low = vec![("market-1".to_string(), dec!(0.01))];

    // First evaluation starts recovery timer
    controller.evaluate(&iirs_low, &prices_low, 0);
    // Second evaluation should complete recovery (hold_secs=0)
    let level = controller.evaluate(&iirs_low, &prices_low, 0);

    assert_eq!(level, RiskLevel::L1Normal, "L2 should recover to L1 when conditions clear");
}

#[test]
fn test_l3_requires_manual_recovery() {
    let mut controller = RiskController::new(&test_risk_config());
    controller.set_total_capital(dec!(10000));

    // Trigger L3
    let iirs = vec![("market-1".to_string(), dec!(0.80))];
    let prices = vec![("market-1".to_string(), dec!(0.01))];
    controller.evaluate(&iirs, &prices, 0);
    assert_eq!(controller.level(), RiskLevel::L3Emergency);

    // Even with good conditions, stays L3
    let iirs_good = vec![("market-1".to_string(), dec!(0.1))];
    let level = controller.evaluate(&iirs_good, &prices, 0);
    assert_eq!(level, RiskLevel::L3Emergency, "L3 should NOT auto-recover");

    // Manual recovery
    controller.manual_recover();
    assert_eq!(controller.level(), RiskLevel::L1Normal, "Manual recovery should return to L1");
}

#[test]
fn test_ghost_fill_detection() {
    let mut controller = RiskController::new(&test_risk_config());

    // Register our cancel
    controller.register_cancel("order-1".to_string());
    assert!(controller.is_our_cancel("order-1"));

    // Unknown cancel → ghost fill
    assert!(!controller.is_our_cancel("order-unknown"));
}

#[test]
fn test_ghost_fills_trigger_l3() {
    let mut controller = RiskController::new(&test_risk_config());
    controller.set_total_capital(dec!(10000));

    // Record 3 ghost fills (threshold)
    controller.record_ghost_fill();
    assert_eq!(controller.level(), RiskLevel::L1Normal); // Not yet
    controller.record_ghost_fill();
    assert_eq!(controller.level(), RiskLevel::L1Normal); // Not yet
    controller.record_ghost_fill();
    assert_eq!(controller.level(), RiskLevel::L3Emergency, "3 ghost fills should trigger L3");
}
