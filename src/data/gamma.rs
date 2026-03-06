use std::collections::HashMap;

use alloy::primitives::{B256, U256};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tracing::{debug, info, warn};

use rust_decimal::Decimal;

use polymarket_client_sdk::gamma;
use polymarket_client_sdk::gamma::types::request::{MarketsRequest, SearchRequest};

/// Market metadata fetched from Gamma API.
pub struct MarketMetadata {
    pub end_date: Option<DateTime<Utc>>,
    pub condition_id: Option<B256>,
}

/// A search result from the Gamma API — one tradeable market token.
#[derive(Debug, Clone)]
pub struct GammaSearchResult {
    /// Condition ID (our market_id)
    pub condition_id: String,
    /// YES token ID for CLOB trading
    pub token_id: String,
    /// Human-readable question/title
    pub question: String,
    /// Current best bid/ask midpoint (if available)
    pub midpoint: Option<Decimal>,
    /// Settlement end date
    pub end_date: Option<DateTime<Utc>>,
    /// Max spread for LP rewards qualification
    pub rewards_max_spread: Option<Decimal>,
    /// Min order size for LP rewards
    pub rewards_min_size: Option<Decimal>,
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

    /// Fetch metadata for all configured markets using token_ids.
    ///
    /// The Gamma API `/markets/{id}` endpoint requires a numeric ID,
    /// not a condition_id hash. Instead we use `GET /markets?clob_token_ids=...`
    /// which accepts the CLOB token IDs we have in config.
    ///
    /// Returns two maps keyed by market_id (condition_id):
    /// - market_id → end_date (for TF time factor)
    /// - market_id → condition_id B256 (for CTF merge)
    pub async fn fetch_all_metadata(
        &self,
        markets: &[(String, String)],  // Vec of (market_id, token_id)
    ) -> (HashMap<String, DateTime<Utc>>, HashMap<String, B256>) {
        let mut times = HashMap::new();
        let mut conditions = HashMap::new();

        // Build token_id → market_id lookup
        let mut token_to_market: HashMap<String, String> = HashMap::new();
        let mut token_ids: Vec<U256> = Vec::new();
        for (market_id, token_id) in markets {
            if let Ok(u) = token_id.parse::<U256>() {
                token_ids.push(u);
                token_to_market.insert(token_id.clone(), market_id.clone());
            } else {
                warn!("Invalid token_id for market {market_id}: {token_id}");
            }
        }

        if token_ids.is_empty() {
            warn!("No valid token_ids to query Gamma API");
            return (times, conditions);
        }

        let request = MarketsRequest::builder()
            .clob_token_ids(token_ids)
            .build();

        match self.client.markets(&request).await {
            Ok(gamma_markets) => {
                for gm in &gamma_markets {
                    // Match Gamma market back to our market_id via clob_token_ids
                    let market_id = gm.clob_token_ids.as_ref()
                        .and_then(|ids| {
                            ids.iter()
                                .find_map(|tid| token_to_market.get(&tid.to_string()))
                        })
                        .cloned();

                    let Some(market_id) = market_id else {
                        debug!("Gamma market {} has no matching token_id in config", gm.question.as_deref().unwrap_or("unknown"));
                        continue;
                    };

                    if let Some(end_date) = gm.end_date {
                        debug!("Market {market_id}: end_date = {end_date}");
                        times.insert(market_id.clone(), end_date);
                    }
                    if let Some(condition_id) = gm.condition_id {
                        debug!("Market {market_id}: condition_id = {condition_id}");
                        conditions.insert(market_id.clone(), condition_id);
                    }
                }
            }
            Err(e) => {
                warn!("Gamma API markets query failed: {e:#}. Settlement times and condition_ids unavailable.");
            }
        }

        info!(
            "Fetched metadata: settlement={}/{}, condition_ids={}/{} markets",
            times.len(), markets.len(),
            conditions.len(), markets.len(),
        );
        (times, conditions)
    }

    /// Search for active markets by text query via the Gamma public-search API.
    ///
    /// Returns up to `limit` results, filtering for active (non-closed) markets
    /// that have CLOB token IDs and valid condition IDs for trading.
    pub async fn search_markets(&self, query: &str, limit: i32) -> Result<Vec<GammaSearchResult>> {
        let request = SearchRequest::builder()
            .q(query)
            .limit_per_type(limit)
            .build();

        let search_results = self.client.search(&request).await
            .context("Gamma search API request failed")?;

        let mut results = Vec::new();

        let Some(events) = search_results.events else {
            debug!("Search returned no events for query '{query}'");
            return Ok(results);
        };

        for event in &events {
            let Some(markets) = &event.markets else {
                continue;
            };

            for market in markets {
                // Skip closed/inactive markets
                if market.closed.unwrap_or(false) || !market.active.unwrap_or(true) {
                    continue;
                }

                // Must have a condition_id (= our market_id)
                let Some(condition_id) = &market.condition_id else {
                    continue;
                };

                // Must have CLOB token IDs for trading
                let Some(clob_token_ids) = &market.clob_token_ids else {
                    continue;
                };
                let Some(first_token) = clob_token_ids.first() else {
                    continue;
                };

                // Calculate midpoint from outcome_prices if available
                let midpoint = market.outcome_prices.as_ref().and_then(|prices| {
                    prices.first().copied()
                });

                let question = market.question.clone().unwrap_or_else(|| {
                    event.title.clone().unwrap_or_else(|| "Unknown".to_string())
                });

                results.push(GammaSearchResult {
                    condition_id: format!("{condition_id:#x}"),
                    token_id: first_token.to_string(),
                    question,
                    midpoint,
                    end_date: market.end_date,
                    rewards_max_spread: market.rewards_max_spread,
                    rewards_min_size: market.rewards_min_size,
                });
            }
        }

        info!(
            "Search '{}': found {} tradeable markets from {} events",
            query, results.len(), events.len()
        );
        Ok(results)
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
