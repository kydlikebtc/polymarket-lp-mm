use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use polymarket_client_sdk::gamma;
use polymarket_client_sdk::gamma::types::request::MarketByIdRequest;

/// Gamma API client for market metadata (end dates, settlement info).
pub struct GammaClient {
    client: gamma::Client,
}

impl GammaClient {
    pub fn new(base_url: &str) -> Result<Self> {
        let client = gamma::Client::new(base_url)
            .context("Failed to create Gamma API client")?;
        Ok(Self { client })
    }

    /// Fetch end_date for a single market.
    pub async fn fetch_end_date(
        &self,
        market_id: &str,
    ) -> Result<Option<DateTime<Utc>>> {
        let request = MarketByIdRequest::builder()
            .id(market_id)
            .build();

        let market = self
            .client
            .market_by_id(&request)
            .await
            .with_context(|| format!("Gamma API: failed to fetch market {market_id}"))?;

        Ok(market.end_date)
    }

    /// Fetch end_dates for all configured markets.
    /// Returns a map of market_id → end_date (only for markets with known dates).
    pub async fn fetch_all_end_dates(
        &self,
        market_ids: &[String],
    ) -> HashMap<String, DateTime<Utc>> {
        let mut times = HashMap::new();

        for id in market_ids {
            match self.fetch_end_date(id).await {
                Ok(Some(end_date)) => {
                    debug!("Market {id}: end_date = {end_date}");
                    times.insert(id.clone(), end_date);
                }
                Ok(None) => {
                    warn!("Market {id}: no end_date available from Gamma API");
                }
                Err(e) => {
                    warn!("Market {id}: Gamma API error: {e:#}");
                }
            }
        }

        info!(
            "Fetched settlement times for {}/{} markets",
            times.len(),
            market_ids.len()
        );
        times
    }
}

/// Compute hours remaining until settlement.
/// Returns None if end_date is in the past.
pub fn hours_until(end_date: &DateTime<Utc>) -> Option<f64> {
    let duration = *end_date - Utc::now();
    let hours = duration.num_seconds() as f64 / 3600.0;
    if hours <= 0.0 {
        None
    } else {
        Some(hours)
    }
}
