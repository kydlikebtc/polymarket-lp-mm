use anyhow::{Context, Result};
use tracing::info;

use crate::config::AppConfig;

/// Wrapper around the Polymarket SDK CLOB client.
/// In MVP, we use the official `polymarket-client-sdk` for API calls.
/// This module provides initialization and helper methods.
pub struct ClobClient {
    pub api_key: String,
    pub api_secret: String,
    pub api_passphrase: String,
    pub private_key: String,
    pub base_url: String,
    pub http: reqwest::Client,
}

impl ClobClient {
    /// Check balance to validate API credentials
    pub async fn check_balance(&self) -> Result<()> {
        // TODO: Integrate with polymarket-client-sdk once we verify API
        // For now, a basic health check
        let url = format!("{}/health", self.base_url);
        let resp = self.http.get(&url).send().await?;
        if resp.status().is_success() {
            info!("CLOB API health check passed");
            Ok(())
        } else {
            anyhow::bail!(
                "CLOB API health check failed: status={}",
                resp.status()
            );
        }
    }
}

/// Create and validate a CLOB client from config
pub async fn create_clob_client(config: &AppConfig) -> Result<ClobClient> {
    let api_key = std::env::var("POLYMARKET_API_KEY")
        .context("POLYMARKET_API_KEY not set in environment")?;
    let api_secret = std::env::var("POLYMARKET_API_SECRET")
        .context("POLYMARKET_API_SECRET not set in environment")?;
    let api_passphrase = std::env::var("POLYMARKET_API_PASSPHRASE")
        .context("POLYMARKET_API_PASSPHRASE not set in environment")?;
    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
        .context("POLYMARKET_PRIVATE_KEY not set in environment")?;

    let client = ClobClient {
        api_key,
        api_secret,
        api_passphrase,
        private_key,
        base_url: config.api.clob_base_url.clone(),
        http: reqwest::Client::new(),
    };

    client
        .check_balance()
        .await
        .context("Failed to validate CLOB API credentials")?;

    Ok(client)
}
