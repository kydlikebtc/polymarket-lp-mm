use rust_decimal::Decimal;
use rust_decimal_macros::dec;

// We test the pricing logic by importing from the crate
use polymarket_mm::config::*;
use polymarket_mm::data::state::OrderSide;
use polymarket_mm::pricing::PricingEngine;
use polymarket_mm::risk::RiskLevel;

fn test_pricing_config() -> PricingConfig {
    PricingConfig {
        layers: vec![
            LayerConfig {
                distance: dec!(0.005), // 0.5 cents
                capital_fraction: dec!(0.20),
            },
            LayerConfig {
                distance: dec!(0.015), // 1.5 cents
                capital_fraction: dec!(0.40),
            },
            LayerConfig {
                distance: dec!(0.025), // 2.5 cents
                capital_fraction: dec!(0.40),
            },
        ],
        base_half_spread: dec!(0.005),
        vaf_min: dec!(0.8),
        vaf_max: dec!(5.0),
        skew_factor: dec!(0.02),
        requote_threshold: dec!(0.005),
        requote_interval_secs: 30,
    }
}

fn test_market_config() -> MarketConfig {
    MarketConfig {
        market_id: "test-market-1".to_string(),
        token_id: "test-token-1".to_string(),
        name: "Test Market".to_string(),
        max_incentive_spread: dec!(0.03),
        min_size: dec!(5.0),
    }
}

#[test]
fn test_generate_quotes_l1_normal() {
    let engine = PricingEngine::new(&test_pricing_config());
    let market = test_market_config();

    let orders = engine.generate_quotes(
        &market,
        dec!(0.50),     // midpoint
        dec!(0.0),      // iir (balanced)
        dec!(1.0),      // vaf (normal)
        dec!(1.0),      // tf (normal)
        dec!(500.0),    // per-market capital
        RiskLevel::L1Normal,
    );

    // 3 layers × 2 sides = 6 orders
    assert_eq!(orders.len(), 6, "Expected 6 orders (3 layers × 2 sides)");

    let bids: Vec<_> = orders.iter().filter(|o| matches!(o.side, OrderSide::Buy)).collect();
    let asks: Vec<_> = orders.iter().filter(|o| matches!(o.side, OrderSide::Sell)).collect();

    assert_eq!(bids.len(), 3, "Expected 3 bid orders");
    assert_eq!(asks.len(), 3, "Expected 3 ask orders");

    // Inner layer bid should be at or just below midpoint
    // Note: 0.50 - 0.005 = 0.495, which rounds to 0.50 at 2dp
    let inner_bid = bids.iter().max_by_key(|o| o.price).unwrap();
    assert!(
        inner_bid.price >= dec!(0.49) && inner_bid.price <= dec!(0.50),
        "Inner bid should be near midpoint, got {}",
        inner_bid.price
    );

    // Inner layer ask should be at or just above midpoint
    let inner_ask = asks.iter().min_by_key(|o| o.price).unwrap();
    assert!(
        inner_ask.price >= dec!(0.50) && inner_ask.price <= dec!(0.51),
        "Inner ask should be near midpoint, got {}",
        inner_ask.price
    );

    // Outer layer should be further from midpoint
    let outer_bid = bids.iter().min_by_key(|o| o.price).unwrap();
    assert!(
        outer_bid.price < inner_bid.price,
        "Outer bid ({}) should be below inner bid ({})",
        outer_bid.price, inner_bid.price
    );
}

#[test]
fn test_no_orders_in_l3() {
    let engine = PricingEngine::new(&test_pricing_config());
    let market = test_market_config();

    let orders = engine.generate_quotes(
        &market,
        dec!(0.50),
        dec!(0.0),
        dec!(1.0),
        dec!(1.0),
        dec!(500.0),
        RiskLevel::L3Emergency,
    );

    assert!(orders.is_empty(), "L3 should produce zero orders");
}

#[test]
fn test_l2_reduces_size_and_widens_spread() {
    let engine = PricingEngine::new(&test_pricing_config());
    let market = test_market_config();

    let l1_orders = engine.generate_quotes(
        &market, dec!(0.50), dec!(0.0), dec!(1.0), dec!(1.0),
        dec!(500.0), RiskLevel::L1Normal,
    );

    let l2_orders = engine.generate_quotes(
        &market, dec!(0.50), dec!(0.0), dec!(1.0), dec!(1.0),
        dec!(500.0), RiskLevel::L2Warning,
    );

    // L2 should have same number of orders but smaller sizes
    assert_eq!(l1_orders.len(), l2_orders.len());

    let l1_total_size: Decimal = l1_orders.iter().map(|o| o.size).sum();
    let l2_total_size: Decimal = l2_orders.iter().map(|o| o.size).sum();

    assert!(
        l2_total_size < l1_total_size,
        "L2 total size ({l2_total_size}) should be smaller than L1 ({l1_total_size})"
    );

    // L2 inner bid should be further from midpoint (wider spread)
    let l1_inner_bid = l1_orders.iter()
        .filter(|o| matches!(o.side, OrderSide::Buy))
        .max_by_key(|o| o.price)
        .unwrap();
    let l2_inner_bid = l2_orders.iter()
        .filter(|o| matches!(o.side, OrderSide::Buy))
        .max_by_key(|o| o.price)
        .unwrap();

    assert!(
        l2_inner_bid.price < l1_inner_bid.price,
        "L2 inner bid ({}) should be lower (wider) than L1 ({})",
        l2_inner_bid.price, l1_inner_bid.price
    );
}

#[test]
fn test_skewing_with_positive_iir() {
    let engine = PricingEngine::new(&test_pricing_config());
    let market = test_market_config();

    // Balanced
    let balanced = engine.generate_quotes(
        &market, dec!(0.50), dec!(0.0), dec!(1.0), dec!(1.0),
        dec!(500.0), RiskLevel::L1Normal,
    );

    // Positive IIR (holding too much YES) → prices shift down
    let skewed = engine.generate_quotes(
        &market, dec!(0.50), dec!(0.6), dec!(1.0), dec!(1.0),
        dec!(500.0), RiskLevel::L1Normal,
    );

    let balanced_inner_ask = balanced.iter()
        .filter(|o| matches!(o.side, OrderSide::Sell))
        .min_by_key(|o| o.price)
        .unwrap();
    let skewed_inner_ask = skewed.iter()
        .filter(|o| matches!(o.side, OrderSide::Sell))
        .min_by_key(|o| o.price)
        .unwrap();

    // With positive IIR, skew is negative → ask price moves lower (easier to sell YES)
    assert!(
        skewed_inner_ask.price < balanced_inner_ask.price,
        "Positive IIR should push ask lower: skewed={}, balanced={}",
        skewed_inner_ask.price, balanced_inner_ask.price
    );
}

#[test]
fn test_qscore_estimation() {
    let engine = PricingEngine::new(&test_pricing_config());
    let market = test_market_config();

    let orders = engine.generate_quotes(
        &market, dec!(0.50), dec!(0.0), dec!(1.0), dec!(1.0),
        dec!(500.0), RiskLevel::L1Normal,
    );

    let q = engine.estimate_qscore(&orders, dec!(0.50), dec!(0.03));

    assert!(
        q > Decimal::ZERO,
        "Q-Score should be positive for valid dual-side orders"
    );

    // Single-side should have 1/3 the score
    let bids_only: Vec<_> = orders.iter()
        .filter(|o| matches!(o.side, OrderSide::Buy))
        .cloned()
        .collect();
    let q_single = engine.estimate_qscore(&bids_only, dec!(0.50), dec!(0.03));

    // q_single should be approximately q / 3 (due to penalty)
    // Not exactly because bid/ask have different sizes
    assert!(
        q_single < q,
        "Single-side Q ({q_single}) should be less than dual-side Q ({q})"
    );
}

#[test]
fn test_time_factor() {
    let engine = PricingEngine::new(&test_pricing_config());

    assert_eq!(engine.compute_tf(None), dec!(1.0));
    assert_eq!(engine.compute_tf(Some(48.0)), dec!(1.0));
    assert_eq!(engine.compute_tf(Some(20.0)), dec!(1.5));
    assert_eq!(engine.compute_tf(Some(10.0)), dec!(2.0));
    assert_eq!(engine.compute_tf(Some(4.0)), dec!(3.0));
    assert_eq!(engine.compute_tf(Some(1.0)), dec!(0.0)); // Stop market making
}

#[test]
fn test_prices_within_bounds() {
    let engine = PricingEngine::new(&test_pricing_config());
    let market = test_market_config();

    // Test with extreme midpoints
    for midpoint in [dec!(0.02), dec!(0.05), dec!(0.50), dec!(0.95), dec!(0.98)] {
        let orders = engine.generate_quotes(
            &market, midpoint, dec!(0.0), dec!(1.0), dec!(1.0),
            dec!(500.0), RiskLevel::L1Normal,
        );

        for order in &orders {
            assert!(
                order.price >= dec!(0.01) && order.price <= dec!(0.99),
                "Order price {} out of bounds [0.01, 0.99] for midpoint={}",
                order.price, midpoint
            );
        }
    }
}
