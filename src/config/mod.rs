use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::info;

/// Top-level application configuration
#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub markets: Vec<MarketConfig>,
    pub capital: CapitalConfig,
    pub pricing: PricingConfig,
    pub position: PositionConfig,
    pub risk: RiskConfig,
    pub execution: ExecutionConfig,
    pub api: ApiConfig,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct MarketConfig {
    pub market_id: String,
    pub token_id: String,
    /// Human-readable name for logging
    pub name: String,
    /// Max incentive spread from Gamma API (e.g., 0.03 = 3 cents)
    pub max_incentive_spread: Decimal,
    /// Min order size for Q-Score qualification
    pub min_size: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CapitalConfig {
    /// Total capital in USDC
    pub total_capital: Decimal,
    /// Max fraction of total capital per single market (e.g., 0.20 = 20%)
    pub max_per_market_fraction: Decimal,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct PricingConfig {
    /// Ladder layers configuration
    pub layers: Vec<LayerConfig>,
    /// Base half-spread in price units (e.g., 0.005 = 0.5 cents)
    pub base_half_spread: Decimal,
    /// VAF clamp bounds
    pub vaf_min: Decimal,
    pub vaf_max: Decimal,
    /// Skew factor: IIR units → price shift (e.g., 0.02)
    pub skew_factor: Decimal,
    /// Price movement threshold to trigger re-quote (e.g., 0.005 = 0.5 cents)
    pub requote_threshold: Decimal,
    /// Timer-based re-quote interval in seconds
    pub requote_interval_secs: u64,
    /// Baseline daily volatility for VAF normalization (e.g., 0.025 = 2.5%)
    pub baseline_volatility: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LayerConfig {
    /// Distance from midpoint in price units (e.g., 0.005 = 0.5 cents)
    pub distance: Decimal,
    /// Capital fraction per side for this layer (e.g., 0.20 = 20%)
    pub capital_fraction: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PositionConfig {
    /// IIR threshold for L2 escalation (e.g., 0.5)
    pub iir_medium_threshold: Decimal,
    /// IIR threshold for L3 escalation (e.g., 0.75)
    pub iir_extreme_threshold: Decimal,
    /// Minimum merge size in USDC
    pub min_merge_size: Decimal,
    /// Merge cooldown in seconds
    pub merge_cooldown_secs: u64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct RiskConfig {
    // L2 thresholds
    pub l2_iir_threshold: Decimal,       // 0.5
    pub l2_price_change_5min: Decimal,   // 0.10
    pub l2_daily_loss_pct: Decimal,      // 0.03
    pub l2_ws_disconnect_secs: u64,      // 30
    // L3 thresholds
    pub l3_iir_threshold: Decimal,       // 0.75
    pub l3_price_change_5min: Decimal,   // 0.20
    pub l3_daily_loss_pct: Decimal,      // 0.08
    pub l3_ghost_fill_count: u32,        // 3
    pub l3_ghost_fill_window_secs: u64,  // 1800
    pub l2_timeout_to_l3_secs: u64,     // 7200
    // L2 recovery
    pub l2_recovery_iir: Decimal,        // 0.4
    pub l2_recovery_price_change: Decimal, // 0.05
    pub l2_recovery_hold_secs: u64,      // 300
    // L2 shrink factor
    pub l2_size_multiplier: Decimal,     // 0.5
    pub l2_spread_multiplier: Decimal,   // 1.5
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionConfig {
    /// Max orders per batch (Polymarket limit = 15)
    pub batch_size: usize,
    /// Retry config
    pub max_retries: u32,
    pub base_retry_delay_ms: u64,
    pub max_retry_delay_ms: u64,
    /// Cancel confirmation timeout in milliseconds
    pub cancel_confirm_timeout_ms: u64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct ApiConfig {
    pub clob_base_url: String,
    pub gamma_base_url: String,
    pub ws_market_url: String,
    pub ws_user_url: String,
    /// Polygon RPC URL for merge/redeem operations
    pub polygon_rpc_url: String,
    /// CTF contract address
    pub ctf_contract: String,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let config_path = std::env::var("CONFIG_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("config.toml"));

        info!("Loading config from: {}", config_path.display());

        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

        let config: AppConfig =
            toml::from_str(&content).context("Failed to parse config.toml")?;

        config.validate()?;

        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.markets.is_empty(), "At least one market must be configured");
        anyhow::ensure!(
            self.markets.len() <= 3,
            "MVP supports at most 3 markets, got {}",
            self.markets.len()
        );
        anyhow::ensure!(
            self.capital.total_capital > Decimal::ZERO,
            "Total capital must be positive"
        );
        anyhow::ensure!(
            self.execution.batch_size <= 15,
            "Polymarket batch limit is 15, got {}",
            self.execution.batch_size
        );

        let mut total_fraction = Decimal::ZERO;
        for (i, layer) in self.pricing.layers.iter().enumerate() {
            anyhow::ensure!(
                layer.distance > Decimal::ZERO,
                "Layer {i} distance must be positive"
            );
            anyhow::ensure!(
                layer.capital_fraction > Decimal::ZERO && layer.capital_fraction <= Decimal::ONE,
                "Layer {i} capital_fraction must be in (0, 1]"
            );
            total_fraction += layer.capital_fraction;
        }
        anyhow::ensure!(
            total_fraction <= Decimal::ONE,
            "Sum of layer capital_fractions ({total_fraction}) must be <= 1.0"
        );

        // Risk parameter sanity checks
        let r = &self.risk;
        anyhow::ensure!(
            r.l3_iir_threshold > r.l2_iir_threshold,
            "l3_iir_threshold ({}) must be > l2_iir_threshold ({})",
            r.l3_iir_threshold, r.l2_iir_threshold
        );
        anyhow::ensure!(
            r.l3_price_change_5min > r.l2_price_change_5min,
            "l3_price_change_5min ({}) must be > l2_price_change_5min ({})",
            r.l3_price_change_5min, r.l2_price_change_5min
        );
        anyhow::ensure!(
            r.l3_daily_loss_pct > r.l2_daily_loss_pct,
            "l3_daily_loss_pct ({}) must be > l2_daily_loss_pct ({})",
            r.l3_daily_loss_pct, r.l2_daily_loss_pct
        );
        anyhow::ensure!(
            r.l3_daily_loss_pct <= Decimal::ONE,
            "l3_daily_loss_pct ({}) must be <= 1.0",
            r.l3_daily_loss_pct
        );
        anyhow::ensure!(
            r.l2_ws_disconnect_secs > 0,
            "l2_ws_disconnect_secs must be > 0"
        );
        anyhow::ensure!(
            self.capital.max_per_market_fraction > Decimal::ZERO
                && self.capital.max_per_market_fraction <= Decimal::ONE,
            "max_per_market_fraction ({}) must be in (0, 1.0]",
            self.capital.max_per_market_fraction
        );
        anyhow::ensure!(
            self.execution.max_retries > 0,
            "execution.max_retries must be > 0"
        );

        // Pricing parameter bounds
        let p = &self.pricing;
        anyhow::ensure!(
            p.baseline_volatility > Decimal::ZERO,
            "baseline_volatility ({}) must be > 0",
            p.baseline_volatility
        );
        anyhow::ensure!(
            p.base_half_spread >= Decimal::ZERO,
            "base_half_spread ({}) must be >= 0",
            p.base_half_spread
        );
        anyhow::ensure!(
            p.vaf_min > Decimal::ZERO && p.vaf_min < p.vaf_max,
            "vaf_min ({}) must be > 0 and < vaf_max ({})",
            p.vaf_min, p.vaf_max
        );
        anyhow::ensure!(
            p.requote_threshold > Decimal::ZERO,
            "requote_threshold ({}) must be > 0",
            p.requote_threshold
        );
        anyhow::ensure!(
            p.skew_factor >= Decimal::ZERO,
            "skew_factor ({}) must be >= 0",
            p.skew_factor
        );

        // Execution bounds
        anyhow::ensure!(
            self.execution.cancel_confirm_timeout_ms > 0,
            "cancel_confirm_timeout_ms must be > 0"
        );

        // API URL scheme validation (R6-5: also validate gamma and polygon URLs)
        anyhow::ensure!(
            self.api.clob_base_url.starts_with("https://"),
            "clob_base_url must use HTTPS"
        );
        anyhow::ensure!(
            self.api.gamma_base_url.starts_with("https://"),
            "gamma_base_url must use HTTPS"
        );
        anyhow::ensure!(
            self.api.polygon_rpc_url.starts_with("https://"),
            "polygon_rpc_url must use HTTPS"
        );
        anyhow::ensure!(
            self.api.ws_market_url.starts_with("wss://"),
            "ws_market_url must use WSS"
        );
        anyhow::ensure!(
            self.api.ws_user_url.starts_with("wss://"),
            "ws_user_url must use WSS"
        );

        Ok(())
    }

    /// Per-market capital allocation
    pub fn per_market_capital(&self) -> Decimal {
        let market_count = Decimal::from(self.markets.len() as u64);
        let max_per_market = self.capital.total_capital * self.capital.max_per_market_fraction;
        let even_split = self.capital.total_capital / market_count;
        max_per_market.min(even_split)
    }
}
