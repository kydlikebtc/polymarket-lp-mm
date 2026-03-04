use rust_decimal_macros::dec;

use polymarket_mm::config::PositionConfig;
use polymarket_mm::data::state::PositionRecord;
use polymarket_mm::position::{PositionAction, PositionManager};

fn test_position_config() -> PositionConfig {
    PositionConfig {
        iir_medium_threshold: dec!(0.5),
        iir_extreme_threshold: dec!(0.75),
        min_merge_size: dec!(100),
        merge_cooldown_secs: 300,
    }
}

/// Helper: build PositionRecord with allocated_capital=$100 for clean math.
/// IIR = (yes_value - no_value) / allocated_capital
fn make_position(yes_shares: f64, no_shares: f64, midpoint: f64) -> PositionRecord {
    let yes = rust_decimal::Decimal::try_from(yes_shares).unwrap();
    let no = rust_decimal::Decimal::try_from(no_shares).unwrap();
    let mid = rust_decimal::Decimal::try_from(midpoint).unwrap();

    PositionRecord {
        market_id: "test-market".to_string(),
        yes_shares: yes,
        no_shares: no,
        yes_value: yes * mid,
        no_value: no * (dec!(1.0) - mid),
        allocated_capital: dec!(100),
        updated_at: chrono::Utc::now(),
    }
}

#[test]
fn test_iir_calculation() {
    // Balanced position: (50 - 50) / 100 = 0
    let pos = make_position(100.0, 100.0, 0.5);
    assert_eq!(pos.iir(), dec!(0));

    // All YES: capital-based → 50/100 = 0.5
    let pos_yes = make_position(100.0, 0.0, 0.5);
    assert_eq!(pos_yes.iir(), dec!(0.5));

    // All NO: capital-based → -50/100 = -0.5
    let pos_no = make_position(0.0, 100.0, 0.5);
    assert_eq!(pos_no.iir(), dec!(-0.5));

    // Empty position
    let pos_empty = make_position(0.0, 0.0, 0.5);
    assert_eq!(pos_empty.iir(), dec!(0));

    // Clamping: 150/100 = 1.5, clamped to 1.0
    let pos_over = make_position(300.0, 0.0, 0.5);
    assert_eq!(pos_over.iir(), dec!(1));
}

#[test]
fn test_balanced_position_no_escalation() {
    let manager = PositionManager::new(&test_position_config());
    let pos = make_position(100.0, 100.0, 0.5);

    let actions = manager.evaluate(&pos);

    // Should have merge action (since both sides have 100 shares) but no escalation
    let has_merge = actions
        .iter()
        .any(|a| matches!(a, PositionAction::TriggerMerge { .. }));
    let has_escalation = actions.iter().any(|a| {
        matches!(
            a,
            PositionAction::EscalateL2 { .. } | PositionAction::EscalateL3 { .. }
        )
    });

    assert!(has_merge, "Balanced 100/100 should trigger merge");
    assert!(!has_escalation, "Balanced IIR should not escalate");
}

#[test]
fn test_light_imbalance_no_action() {
    let manager = PositionManager::new(&test_position_config());
    // IIR = 20/100 = 0.2 (below medium threshold 0.5)
    let pos = make_position(40.0, 0.0, 0.5);

    let actions = manager.evaluate(&pos);

    // Light imbalance is handled by PricingEngine::compute_skew,
    // PositionManager should not produce any actions
    assert!(
        actions.is_empty(),
        "Light imbalance should produce no position actions (handled by PricingEngine)"
    );
}

#[test]
fn test_high_imbalance_escalates_l2() {
    let manager = PositionManager::new(&test_position_config());
    // IIR = 60/100 = 0.6 (high imbalance, >= 0.5)
    let pos = make_position(120.0, 0.0, 0.5);

    let actions = manager.evaluate(&pos);

    let has_l2 = actions
        .iter()
        .any(|a| matches!(a, PositionAction::EscalateL2 { .. }));

    assert!(has_l2, "High imbalance should escalate to L2");
}

#[test]
fn test_extreme_imbalance_escalates_l3() {
    let manager = PositionManager::new(&test_position_config());
    // IIR = 80/100 = 0.8 (extreme, >= 0.75)
    let pos = make_position(160.0, 0.0, 0.5);

    let actions = manager.evaluate(&pos);

    let has_l3 = actions
        .iter()
        .any(|a| matches!(a, PositionAction::EscalateL3 { .. }));

    assert!(has_l3, "Extreme imbalance should escalate to L3");
}

#[test]
fn test_merge_opportunity_detected() {
    let manager = PositionManager::new(&test_position_config());
    // Both sides have 200 shares, min(200, 200) = 200 >= min_merge_size(100)
    let pos = make_position(200.0, 200.0, 0.5);

    let actions = manager.evaluate(&pos);

    let merge = actions.iter().find_map(|a| {
        if let PositionAction::TriggerMerge { amount, .. } = a {
            Some(*amount)
        } else {
            None
        }
    });

    assert!(merge.is_some(), "Should detect merge opportunity");
    assert_eq!(merge.unwrap(), dec!(200), "Merge amount should be 200");
}

#[test]
fn test_small_position_no_merge() {
    let manager = PositionManager::new(&test_position_config());
    // Both sides have 50 shares, below min_merge_size(100)
    let pos = make_position(50.0, 50.0, 0.5);

    let actions = manager.evaluate(&pos);

    let has_merge = actions
        .iter()
        .any(|a| matches!(a, PositionAction::TriggerMerge { .. }));

    assert!(!has_merge, "Small position should NOT trigger merge");
}

#[test]
fn test_mergeable_amount() {
    let pos = make_position(300.0, 200.0, 0.5);
    assert_eq!(pos.mergeable_amount(), dec!(200));

    let pos2 = make_position(100.0, 500.0, 0.5);
    assert_eq!(pos2.mergeable_amount(), dec!(100));
}
