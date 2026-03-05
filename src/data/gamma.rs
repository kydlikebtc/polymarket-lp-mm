use std::collections::HashMap;

use alloy::primitives::B256;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use polymarket_client_sdk::gamma;
use polymarket_client_sdk::gamma::types::request::MarketByIdRequest;

/// Market metadata fetched from Gamma API.
pub struct MarketMetadata {
    pub end_date: Option<DateTime<Utc>>,
    pub condition_id: Option<B256>,
}

/// Gamma API client for market metadata (end dates, settlement info, condition IDs).
pub struct GammaClient {
    client: gamma::Client,
}

impl GammaClient {
    pub fn new(base_url: &str) -> Result<Self> {
        let client = gamma::Client::new(base_url)
            .context("Failed to create Gamma API client")?;
        Ok(Self { client })
    }

    /// Fetch metadata for a single market (end_date + condition_id).
    pub async fn fetch_market_metadata(
        &self,
        market_id: &str,
    ) -> Result<MarketMetadata> {
        let request = MarketByIdRequest::builder()
            .id(market_id)
            .build();

        let market = self
            .client
            .market_by_id(&request)
            .await
            .with_context(|| format!("Gamma API: failed to fetch market {market_id}"))?;

        Ok(MarketMetadata {
            end_date: market.end_date,
            condition_id: market.condition_id,
        })
    }

    /// Fetch end_date for a single market (convenience wrapper).
    pub async fn fetch_end_date(
        &self,
        market_id: &str,
    ) -> Result<Option<DateTime<Utc>>> {
        let meta = self.fetch_market_metadata(market_id).await?;
        Ok(meta.end_date)
    }

    /// Fetch metadata for all configured markets.
    /// Returns two maps: market_id → end_date and market_id → condition_id.
    pub async fn fetch_all_metadata(
        &self,
        market_ids: &[String],
    ) -> (HashMap<String, DateTime<Utc>>, HashMap<String, B256>) {
        let mut times = HashMap::new();
        let mut conditions = HashMap::new();

        for id in market_ids {
            match self.fetch_market_metadata(id).await {
                Ok(meta) => {
                    if let Some(end_date) = meta.end_date {
                        debug!("Market {id}: end_date = {end_date}");
                        times.insert(id.clone(), end_date);
                    } else {
                        warn!("Market {id}: no end_date available from Gamma API");
                    }
                    if let Some(condition_id) = meta.condition_id {
                        debug!("Market {id}: condition_id = {condition_id}");
                        conditions.insert(id.clone(), condition_id);
                    } else {
                        warn!("Market {id}: no condition_id available from Gamma API");
                    }
                }
                Err(e) => {
                    warn!("Market {id}: Gamma API error: {e:#}");
                }
            }
        }

        info!(
            "Fetched metadata: settlement={}/{}, condition_ids={}/{} markets",
            times.len(), market_ids.len(),
            conditions.len(), market_ids.len(),
        );
        (times, conditions)
    }

    /// Fetch end_dates for all configured markets (backwards-compatible).
    pub async fn fetch_all_end_dates(
        &self,
        market_ids: &[String],
    ) -> HashMap<String, DateTime<Utc>> {
        let (times, _) = self.fetch_all_metadata(market_ids).await;
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
