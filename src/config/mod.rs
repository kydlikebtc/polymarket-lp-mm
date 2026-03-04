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
    /// IIR thresholds for response tiers
    pub iir_light_threshold: Decimal,   // e.g., 0.3
    pub iir_medium_threshold: Decimal,  // e.g., 0.5 (triggers L2)
    pub iir_extreme_threshold: Decimal, // e.g., 0.75 (triggers L3)
    /// Light skew factor (when |IIR| < light)
    pub light_skew: Decimal,  // e.g., 0.005
    /// Medium skew factor (when light <= |IIR| < medium)
    pub medium_skew: Decimal, // e.g., 0.015
    /// Minimum merge size in USDC
    pub min_merge_size: Decimal, // e.g., 100
    /// Merge cooldown in seconds
    pub merge_cooldown_secs: u64,
}

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

        for (i, layer) in self.pricing.layers.iter().enumerate() {
            anyhow::ensure!(
                layer.distance > Decimal::ZERO,
                "Layer {i} distance must be positive"
            );
            anyhow::ensure!(
                layer.capital_fraction > Decimal::ZERO && layer.capital_fraction <= Decimal::ONE,
                "Layer {i} capital_fraction must be in (0, 1]"
            );
        }

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
