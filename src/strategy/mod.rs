use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, ensure};
use rust_decimal::Decimal;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::{AppConfig, LayerConfig, MarketConfig, PricingConfig};

/// Named strategy profile with its own pricing parameters.
/// Profiles are loaded from configs/strategy-*.toml at startup.
#[derive(Debug, Clone)]
pub struct StrategyProfile {
    pub name: String,
    pub pricing: PricingConfig,
}

/// Per-field overrides that take precedence over the profile defaults.
/// `None` means "use profile default".
#[derive(Debug, Clone, Default)]
pub struct PricingOverrides {
    pub base_half_spread: Option<Decimal>,
    pub skew_factor: Option<Decimal>,
    pub layers: Option<Vec<LayerConfig>>,
    pub baseline_volatility: Option<Decimal>,
    pub vaf_min: Option<Decimal>,
    pub vaf_max: Option<Decimal>,
    pub requote_threshold: Option<Decimal>,
    pub requote_interval_secs: Option<u64>,
}

/// A single strategy instance bound to one market.
#[derive(Debug, Clone)]
pub struct StrategyInstance {
    pub market: MarketConfig,
    pub profile_name: String,
    pub enabled: bool,
    pub capital_allocation: Decimal,
    pub overrides: PricingOverrides,
}

/// Thread-safe registry of all strategy instances and profiles.
/// The orchestrator reads this on each strategy_tick; TUI commands mutate it.
pub struct StrategyRegistry {
    profiles: HashMap<String, StrategyProfile>,
    instances: Vec<StrategyInstance>,
}

/// Thread-safe handle for sharing across tasks.
pub type SharedRegistry = Arc<RwLock<StrategyRegistry>>;

impl StrategyRegistry {
    /// Initialize from AppConfig at startup.
    /// Creates a single "default" profile from the config's pricing section,
    /// and one instance per configured market.
    pub fn from_config(config: &AppConfig) -> Self {
        let default_profile = StrategyProfile {
            name: "default".to_string(),
            pricing: config.pricing.clone(),
        };

        let per_market_capital = config.per_market_capital();

        let instances: Vec<StrategyInstance> = config
            .markets
            .iter()
            .map(|m| StrategyInstance {
                market: m.clone(),
                profile_name: "default".to_string(),
                enabled: true,
                capital_allocation: per_market_capital,
                overrides: PricingOverrides::default(),
            })
            .collect();

        let mut profiles = HashMap::new();
        profiles.insert("default".to_string(), default_profile);

        info!(
            "StrategyRegistry initialized: {} profiles, {} instances",
            profiles.len(),
            instances.len()
        );

        Self {
            profiles,
            instances,
        }
    }

    /// Create a shared handle wrapped in Arc<RwLock>.
    pub fn into_shared(self) -> SharedRegistry {
        Arc::new(RwLock::new(self))
    }

    // ── Query methods ──

    /// Get all enabled instances (for strategy_tick iteration).
    pub fn active_instances(&self) -> Vec<&StrategyInstance> {
        self.instances.iter().filter(|i| i.enabled).collect()
    }

    /// Get all instances (for snapshot/TUI display).
    pub fn all_instances(&self) -> &[StrategyInstance] {
        &self.instances
    }

    /// Get a specific instance by market_id.
    pub fn get_instance(&self, market_id: &str) -> Option<&StrategyInstance> {
        self.instances
            .iter()
            .find(|i| i.market.market_id == market_id)
    }

    /// Get all profile names.
    pub fn profile_names(&self) -> Vec<String> {
        self.profiles.keys().cloned().collect()
    }

    /// Compute the effective PricingConfig for an instance.
    /// Merges the profile's base config with per-instance overrides.
    ///
    /// # Panics
    /// Panics if the registry has zero profiles (should never happen as
    /// `from_config` always creates a "default" profile).
    pub fn effective_pricing(&self, instance: &StrategyInstance) -> PricingConfig {
        let base = self
            .profiles
            .get(&instance.profile_name)
            .map(|p| &p.pricing)
            .unwrap_or_else(|| {
                warn!(
                    "Profile '{}' not found for market={}, using first available",
                    instance.profile_name, instance.market.market_id
                );
                &self
                    .profiles
                    .values()
                    .next()
                    .expect("StrategyRegistry must have at least one profile")
                    .pricing
            });

        let overrides = &instance.overrides;

        PricingConfig {
            base_half_spread: overrides.base_half_spread.unwrap_or(base.base_half_spread),
            skew_factor: overrides.skew_factor.unwrap_or(base.skew_factor),
            layers: overrides
                .layers
                .clone()
                .unwrap_or_else(|| base.layers.clone()),
            baseline_volatility: overrides
                .baseline_volatility
                .unwrap_or(base.baseline_volatility),
            vaf_min: overrides.vaf_min.unwrap_or(base.vaf_min),
            vaf_max: overrides.vaf_max.unwrap_or(base.vaf_max),
            requote_threshold: overrides
                .requote_threshold
                .unwrap_or(base.requote_threshold),
            requote_interval_secs: overrides
                .requote_interval_secs
                .unwrap_or(base.requote_interval_secs),
        }
    }

    // ── Mutation methods (called via TuiCommand handling) ──

    /// Toggle a market's enabled/disabled state.
    /// Returns the new enabled state, or None if market not found.
    pub fn toggle_market(&mut self, market_id: &str) -> Option<bool> {
        if let Some(instance) = self
            .instances
            .iter_mut()
            .find(|i| i.market.market_id == market_id)
        {
            instance.enabled = !instance.enabled;
            info!(
                "Market '{}' ({}) toggled to {}",
                instance.market.name,
                market_id,
                if instance.enabled { "ENABLED" } else { "DISABLED" }
            );
            Some(instance.enabled)
        } else {
            warn!("toggle_market: market_id={market_id} not found");
            None
        }
    }

    /// Update strategy parameters for a market.
    pub fn update_strategy(
        &mut self,
        market_id: &str,
        profile_name: Option<String>,
        overrides: Option<PricingOverrides>,
        capital: Option<Decimal>,
    ) -> Result<()> {
        let instance = self
            .instances
            .iter_mut()
            .find(|i| i.market.market_id == market_id)
            .ok_or_else(|| anyhow::anyhow!("Market {market_id} not found in registry"))?;

        if let Some(name) = profile_name {
            ensure!(
                self.profiles.contains_key(&name),
                "Profile '{name}' not found"
            );
            instance.profile_name = name;
        }

        if let Some(ovr) = overrides {
            instance.overrides = ovr;
        }

        if let Some(cap) = capital {
            ensure!(cap > Decimal::ZERO, "Capital must be positive");
            ensure!(
                cap <= Decimal::from(1_000_000u64),
                "Capital exceeds safety limit"
            );
            instance.capital_allocation = cap;
        }

        info!(
            "Strategy updated for market='{}': profile={}, capital={}",
            instance.market.name, instance.profile_name, instance.capital_allocation
        );

        Ok(())
    }

    /// Add a new market with a strategy instance.
    pub fn add_market(
        &mut self,
        market: MarketConfig,
        profile_name: String,
        capital: Decimal,
    ) -> Result<()> {
        // Prevent duplicates
        ensure!(
            !self
                .instances
                .iter()
                .any(|i| i.market.market_id == market.market_id),
            "Market {} already exists in registry",
            market.market_id
        );

        ensure!(
            self.profiles.contains_key(&profile_name),
            "Profile '{profile_name}' not found"
        );

        ensure!(
            self.instances.len() < 10,
            "Maximum 10 markets supported"
        );

        info!(
            "Adding market '{}' ({}) with profile={}, capital={}",
            market.name, market.market_id, profile_name, capital
        );

        self.instances.push(StrategyInstance {
            market,
            profile_name,
            enabled: true,
            capital_allocation: capital,
            overrides: PricingOverrides::default(),
        });

        Ok(())
    }

    /// Remove a market from the registry.
    /// Returns the removed instance, or None if not found.
    pub fn remove_market(&mut self, market_id: &str) -> Option<StrategyInstance> {
        if let Some(pos) = self
            .instances
            .iter()
            .position(|i| i.market.market_id == market_id)
        {
            let removed = self.instances.remove(pos);
            info!(
                "Removed market '{}' ({}) from registry",
                removed.market.name, market_id
            );
            Some(removed)
        } else {
            warn!("remove_market: market_id={market_id} not found");
            None
        }
    }

    /// Add a new profile.
    pub fn add_profile(&mut self, profile: StrategyProfile) {
        info!("Adding profile: {}", profile.name);
        self.profiles.insert(profile.name.clone(), profile);
    }

    /// Load profiles from strategy config files in a directory.
    /// Looks for files matching `strategy-*.toml` and parses their [pricing] section.
    pub fn load_profiles_from_dir(&mut self, dir: &str) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            warn!("Cannot read profiles directory: {dir}");
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            if !filename.starts_with("strategy-") || !filename.ends_with(".toml") {
                continue;
            }

            // Extract profile name: "strategy-conservative.toml" → "conservative"
            let profile_name = filename
                .strip_prefix("strategy-")
                .and_then(|s| s.strip_suffix(".toml"))
                .unwrap_or(filename);

            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    // Parse as a partial config containing [pricing]
                    match toml::from_str::<PartialStrategyConfig>(&content) {
                        Ok(partial) => {
                            let profile = StrategyProfile {
                                name: profile_name.to_string(),
                                pricing: partial.pricing,
                            };
                            info!("Loaded profile '{}' from {}", profile_name, path.display());
                            self.profiles.insert(profile_name.to_string(), profile);
                        }
                        Err(e) => {
                            warn!(
                                "Failed to parse profile from {}: {e:#}",
                                path.display()
                            );
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to read {}: {e:#}", path.display());
                }
            }
        }
    }
}

/// Partial config for loading strategy profiles from TOML files.
/// Only the [pricing] section is required.
#[derive(serde::Deserialize)]
struct PartialStrategyConfig {
    pricing: PricingConfig,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn test_config() -> AppConfig {
        let toml_str = r#"
            [[markets]]
            market_id = "market-1"
            token_id = "token-1"
            name = "Test Market 1"
            max_incentive_spread = 0.03
            min_size = 5

            [[markets]]
            market_id = "market-2"
            token_id = "token-2"
            name = "Test Market 2"
            max_incentive_spread = 0.03
            min_size = 5

            [capital]
            total_capital = 1000
            max_per_market_fraction = 0.5

            [pricing]
            base_half_spread = 0.008
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
            distance = 0.015
            capital_fraction = 0.35

            [position]
            iir_medium_threshold = 0.3
            iir_extreme_threshold = 0.6
            min_merge_size = 50
            merge_cooldown_secs = 300

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
            batch_size = 15
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
            ctf_contract = "0xd6b0d3FBfD10E0D579E1ACf7C8968a4926a3dFFF"
        "#;
        AppConfig::from_toml_str(toml_str).unwrap()
    }

    #[test]
    fn test_from_config_creates_default_profile() {
        let config = test_config();
        let registry = StrategyRegistry::from_config(&config);

        assert_eq!(registry.profiles.len(), 1);
        assert!(registry.profiles.contains_key("default"));
        assert_eq!(registry.instances.len(), 2);
    }

    #[test]
    fn test_active_instances_filters_disabled() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        assert_eq!(registry.active_instances().len(), 2);

        registry.toggle_market("market-1");
        assert_eq!(registry.active_instances().len(), 1);
        assert_eq!(
            registry.active_instances()[0].market.market_id,
            "market-2"
        );
    }

    #[test]
    fn test_toggle_market_returns_new_state() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let result = registry.toggle_market("market-1");
        assert_eq!(result, Some(false)); // was enabled, now disabled

        let result = registry.toggle_market("market-1");
        assert_eq!(result, Some(true)); // was disabled, now enabled

        let result = registry.toggle_market("nonexistent");
        assert_eq!(result, None);
    }

    #[test]
    fn test_effective_pricing_with_no_overrides() {
        let config = test_config();
        let registry = StrategyRegistry::from_config(&config);

        let instance = registry.get_instance("market-1").unwrap();
        let effective = registry.effective_pricing(instance);

        assert_eq!(effective.base_half_spread, config.pricing.base_half_spread);
        assert_eq!(effective.skew_factor, config.pricing.skew_factor);
        assert_eq!(effective.layers.len(), config.pricing.layers.len());
    }

    #[test]
    fn test_effective_pricing_with_overrides() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let overrides = PricingOverrides {
            base_half_spread: Some(dec!(0.012)),
            skew_factor: Some(dec!(0.03)),
            ..Default::default()
        };

        registry
            .update_strategy("market-1", None, Some(overrides), None)
            .unwrap();

        let instance = registry.get_instance("market-1").unwrap();
        let effective = registry.effective_pricing(instance);

        assert_eq!(effective.base_half_spread, dec!(0.012));
        assert_eq!(effective.skew_factor, dec!(0.03));
        // Non-overridden fields should use profile defaults
        assert_eq!(effective.layers.len(), config.pricing.layers.len());
    }

    #[test]
    fn test_add_market() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let new_market = MarketConfig {
            market_id: "market-3".to_string(),
            token_id: "token-3".to_string(),
            name: "Test Market 3".to_string(),
            max_incentive_spread: dec!(0.03),
            min_size: dec!(5),
        };

        registry
            .add_market(new_market, "default".to_string(), dec!(200))
            .unwrap();

        assert_eq!(registry.instances.len(), 3);
        let instance = registry.get_instance("market-3").unwrap();
        assert!(instance.enabled);
        assert_eq!(instance.capital_allocation, dec!(200));
    }

    #[test]
    fn test_add_duplicate_market_fails() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let dup = MarketConfig {
            market_id: "market-1".to_string(),
            token_id: "token-dup".to_string(),
            name: "Duplicate".to_string(),
            max_incentive_spread: dec!(0.03),
            min_size: dec!(5),
        };

        let result = registry.add_market(dup, "default".to_string(), dec!(100));
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_market() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let removed = registry.remove_market("market-1");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().market.name, "Test Market 1");
        assert_eq!(registry.instances.len(), 1);

        // Removing again should return None
        assert!(registry.remove_market("market-1").is_none());
    }

    #[test]
    fn test_update_strategy_capital() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        registry
            .update_strategy("market-1", None, None, Some(dec!(300)))
            .unwrap();

        let instance = registry.get_instance("market-1").unwrap();
        assert_eq!(instance.capital_allocation, dec!(300));
    }

    #[test]
    fn test_update_strategy_invalid_profile() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let result = registry.update_strategy(
            "market-1",
            Some("nonexistent".to_string()),
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_capital_allocation_from_config() {
        let config = test_config();
        let registry = StrategyRegistry::from_config(&config);

        // 2 markets, total=1000, max_fraction=0.5
        // per_market = min(1000*0.5, 1000/2) = min(500, 500) = 500
        let instance = registry.get_instance("market-1").unwrap();
        assert_eq!(instance.capital_allocation, dec!(500));
    }

    #[test]
    fn test_add_market_invalid_profile_fails() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let market = MarketConfig {
            market_id: "market-new".to_string(),
            token_id: "token-new".to_string(),
            name: "New Market".to_string(),
            max_incentive_spread: dec!(0.03),
            min_size: dec!(5),
        };

        let result = registry.add_market(market, "nonexistent_profile".to_string(), dec!(100));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not found")
        );
    }

    #[test]
    fn test_add_market_exceeds_limit() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        // Already have 2 markets, add 8 more to hit the 10-market limit
        for i in 3..=10 {
            let market = MarketConfig {
                market_id: format!("market-{i}"),
                token_id: format!("token-{i}"),
                name: format!("Market {i}"),
                max_incentive_spread: dec!(0.03),
                min_size: dec!(5),
            };
            registry
                .add_market(market, "default".to_string(), dec!(100))
                .unwrap();
        }

        assert_eq!(registry.instances.len(), 10);

        // 11th market should fail
        let market = MarketConfig {
            market_id: "market-11".to_string(),
            token_id: "token-11".to_string(),
            name: "Market 11".to_string(),
            max_incentive_spread: dec!(0.03),
            min_size: dec!(5),
        };
        let result = registry.add_market(market, "default".to_string(), dec!(100));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Maximum 10"));
    }

    #[test]
    fn test_add_profile_and_use() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let aggressive = StrategyProfile {
            name: "aggressive".to_string(),
            pricing: PricingConfig {
                base_half_spread: dec!(0.004),
                skew_factor: dec!(0.01),
                ..config.pricing.clone()
            },
        };
        registry.add_profile(aggressive);

        assert!(registry.profiles.contains_key("aggressive"));

        // Should be able to assign this profile to a market
        registry
            .update_strategy("market-1", Some("aggressive".to_string()), None, None)
            .unwrap();

        let instance = registry.get_instance("market-1").unwrap();
        assert_eq!(instance.profile_name, "aggressive");

        let effective = registry.effective_pricing(instance);
        assert_eq!(effective.base_half_spread, dec!(0.004));
    }

    #[test]
    fn test_profile_names_returns_all() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let names = registry.profile_names();
        assert_eq!(names.len(), 1);
        assert!(names.contains(&"default".to_string()));

        registry.add_profile(StrategyProfile {
            name: "conservative".to_string(),
            pricing: config.pricing.clone(),
        });
        registry.add_profile(StrategyProfile {
            name: "aggressive".to_string(),
            pricing: config.pricing.clone(),
        });

        let names = registry.profile_names();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"default".to_string()));
        assert!(names.contains(&"conservative".to_string()));
        assert!(names.contains(&"aggressive".to_string()));
    }

    #[test]
    fn test_update_strategy_nonexistent_market() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        let result = registry.update_strategy("no-such-market", None, None, Some(dec!(100)));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_remove_market_updates_active_instances() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        assert_eq!(registry.active_instances().len(), 2);

        registry.remove_market("market-1");

        assert_eq!(registry.active_instances().len(), 1);
        assert_eq!(registry.all_instances().len(), 1);
        assert!(registry.get_instance("market-1").is_none());
        assert!(registry.get_instance("market-2").is_some());
    }

    #[test]
    fn test_update_strategy_capital_validation() {
        let config = test_config();
        let mut registry = StrategyRegistry::from_config(&config);

        // Zero capital should fail
        let result = registry.update_strategy("market-1", None, None, Some(dec!(0)));
        assert!(result.is_err());

        // Negative capital should fail
        let result = registry.update_strategy("market-1", None, None, Some(dec!(-100)));
        assert!(result.is_err());

        // Over safety limit should fail
        let result = registry.update_strategy("market-1", None, None, Some(dec!(2_000_000)));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("safety limit"));

        // Valid capital should succeed
        registry
            .update_strategy("market-1", None, None, Some(dec!(999_999)))
            .unwrap();
    }
}
