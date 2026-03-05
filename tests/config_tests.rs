use rust_decimal_macros::dec;

use polymarket_mm::config::AppConfig;

/// Minimal valid TOML for a working config.
const VALID_TOML: &str = r#"
[[markets]]
market_id = "m1"
token_id = "t1"
name = "Test Market"
max_incentive_spread = 0.03
min_size = 5

[capital]
total_capital = 1000
max_per_market_fraction = 0.50

[pricing]
base_half_spread = 0.005
vaf_min = 0.5
vaf_max = 3.0
skew_factor = 0.02
requote_threshold = 0.005
requote_interval_secs = 30
baseline_volatility = 0.025

[[pricing.layers]]
distance = 0.005
capital_fraction = 0.40

[[pricing.layers]]
distance = 0.010
capital_fraction = 0.35

[position]
iir_medium_threshold = 0.5
iir_extreme_threshold = 0.75
min_merge_size = 100
merge_cooldown_secs = 600

[risk]
l2_iir_threshold = 0.5
l2_price_change_5min = 0.10
l2_daily_loss_pct = 0.03
l2_ws_disconnect_secs = 30
l3_iir_threshold = 0.75
l3_price_change_5min = 0.20
l3_daily_loss_pct = 0.08
l3_ghost_fill_count = 3
l3_ghost_fill_window_secs = 1800
l2_timeout_to_l3_secs = 7200
l2_recovery_iir = 0.4
l2_recovery_price_change = 0.05
l2_recovery_hold_secs = 300
l2_size_multiplier = 0.5
l2_spread_multiplier = 1.5

[execution]
batch_size = 10
max_retries = 3
base_retry_delay_ms = 500
max_retry_delay_ms = 5000
cancel_confirm_timeout_ms = 3000

[api]
clob_base_url = "https://clob.polymarket.com"
gamma_base_url = "https://gamma-api.polymarket.com"
ws_market_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market"
ws_user_url = "wss://ws-subscriptions-clob.polymarket.com/ws/user"
polygon_rpc_url = "https://polygon-rpc.com"
ctf_contract = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045"
"#;

// ── Happy path ──

#[test]
fn test_valid_config_loads() {
    let config = AppConfig::from_toml_str(VALID_TOML).expect("Valid TOML should load");
    assert_eq!(config.markets.len(), 1);
    assert_eq!(config.capital.total_capital, dec!(1000));
    assert_eq!(config.pricing.layers.len(), 2);
}

#[test]
fn test_per_market_capital_single_market() {
    let config = AppConfig::from_toml_str(VALID_TOML).unwrap();
    // single market: min(1000*0.50, 1000/1) = 500
    assert_eq!(config.per_market_capital(), dec!(500));
}

// ── Validation failures ──

#[test]
fn test_no_markets_fails() {
    let toml = VALID_TOML.replace(
        "[[markets]]\nmarket_id = \"m1\"\ntoken_id = \"t1\"\nname = \"Test Market\"\nmax_incentive_spread = 0.03\nmin_size = 5",
        "",
    );
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "Config with no markets should fail");
}

#[test]
fn test_zero_capital_fails() {
    let toml = VALID_TOML.replace("total_capital = 1000", "total_capital = 0");
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "Zero capital should fail validation");
}

#[test]
fn test_capital_over_limit_fails() {
    let toml = VALID_TOML.replace("total_capital = 1000", "total_capital = 2000000");
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "Capital over 1M should fail");
}

#[test]
fn test_layer_fractions_over_one_fails() {
    let toml = VALID_TOML
        .replace("capital_fraction = 0.40", "capital_fraction = 0.60")
        .replace("capital_fraction = 0.35", "capital_fraction = 0.50");
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "Layer fractions > 1.0 should fail");
}

#[test]
fn test_l3_threshold_less_than_l2_fails() {
    let toml = VALID_TOML.replace("l3_iir_threshold = 0.75", "l3_iir_threshold = 0.40");
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "L3 threshold < L2 threshold should fail");
}

#[test]
fn test_recovery_above_escalation_fails() {
    let toml = VALID_TOML.replace("l2_recovery_iir = 0.4", "l2_recovery_iir = 0.6");
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "Recovery threshold >= escalation should fail (oscillation risk)");
}

#[test]
fn test_non_https_clob_url_fails() {
    let toml = VALID_TOML.replace(
        "clob_base_url = \"https://clob.polymarket.com\"",
        "clob_base_url = \"http://clob.polymarket.com\"",
    );
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "HTTP clob URL should fail");
}

#[test]
fn test_non_wss_ws_url_fails() {
    let toml = VALID_TOML.replace(
        "ws_market_url = \"wss://ws-subscriptions-clob.polymarket.com/ws/market\"",
        "ws_market_url = \"ws://ws-subscriptions-clob.polymarket.com/ws/market\"",
    );
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "Non-WSS URL should fail");
}

#[test]
fn test_zero_min_size_fails() {
    let toml = VALID_TOML.replace("min_size = 5", "min_size = 0");
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "Zero min_size should fail");
}

#[test]
fn test_zero_max_per_market_fraction_fails() {
    let toml = VALID_TOML.replace("max_per_market_fraction = 0.50", "max_per_market_fraction = 0");
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "Zero fraction should fail");
}

#[test]
fn test_l2_spread_multiplier_below_one_fails() {
    let toml = VALID_TOML.replace("l2_spread_multiplier = 1.5", "l2_spread_multiplier = 0.8");
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "L2 spread multiplier < 1.0 should fail");
}

#[test]
fn test_retry_delay_ordering_fails() {
    let toml = VALID_TOML.replace("max_retry_delay_ms = 5000", "max_retry_delay_ms = 100");
    let err = AppConfig::from_toml_str(&toml);
    assert!(err.is_err(), "max_retry_delay < base_retry_delay should fail");
}
