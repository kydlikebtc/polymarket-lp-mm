use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};
use tracing::{debug, error, info, warn};

use alloy::primitives::Address;

use crate::config::{AppConfig, ExecutionConfig};
use crate::data::rest::ClobClient;
use crate::data::state::{OrderRecord, OrderStatus, SharedState};
use crate::pricing::QuoteOrder;
use crate::risk::RiskController;

use polymarket_client_sdk::auth::Credentials;

pub struct OrderExecutor {
    client: ClobClient,
    config: ExecutionConfig,
}

impl OrderExecutor {
    pub fn new(client: ClobClient, app_config: &AppConfig) -> Self {
        Self {
            client,
            config: app_config.execution.clone(),
        }
    }

    /// Access the underlying CLOB client (for balance queries, etc.)
    pub fn client(&self) -> &ClobClient {
        &self.client
    }

    /// Get API credentials (needed for WebSocket authentication)
    pub fn credentials(&self) -> &Credentials {
        &self.client.credentials
    }

    /// Get wallet address (needed for WebSocket authentication)
    pub fn address(&self) -> Address {
        self.client.address
    }

    /// Cancel all orders for a specific market.
    /// Acquires RiskController lock briefly for ghost fill registration,
    /// then releases before HTTP calls to avoid blocking WS task.
    pub async fn cancel_market_orders(
        &mut self,
        state: &SharedState,
        market_id: &str,
        risk_controller: &Arc<Mutex<RiskController>>,
    ) -> Result<Vec<String>> {
        let order_ids: Vec<String> = state
            .my_orders
            .iter()
            .filter(|entry| {
                entry.market_id == market_id
                    && matches!(entry.status, OrderStatus::Live | OrderStatus::Pending)
            })
            .map(|entry| entry.order_id.clone())
            .collect();

        if order_ids.is_empty() {
            debug!("No active orders to cancel for market={market_id}");
            return Ok(vec![]);
        }

        info!(
            "Cancelling {} orders for market={market_id}",
            order_ids.len()
        );

        // Brief lock: register cancel requests for ghost fill detection, then release
        {
            let mut rc = risk_controller.lock().await;
            for id in &order_ids {
                rc.register_cancel(id.clone());
            }
        }

        // Look up token_id for per-market cancel (API uses token_id, not market_id)
        let token_id = state
            .token_to_market
            .iter()
            .find(|entry| entry.value() == market_id)
            .map(|entry| entry.key().clone());

        // Call real API with retry (NO lock held during HTTP)
        match &token_id {
            Some(tid) => self.cancel_market_with_retry(tid).await?,
            None => {
                warn!("No token_id found for market={market_id}, using cancel_all as fallback");
                self.client.cancel_all_orders().await?;
            }
        }

        // Wait for confirmation via WebSocket (with timeout, NO lock held)
        let timeout = Duration::from_millis(self.config.cancel_confirm_timeout_ms);
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            let all_cancelled = order_ids.iter().all(|id| {
                state
                    .my_orders
                    .get(id)
                    .map(|o| o.status == OrderStatus::Canceled)
                    .unwrap_or(true)
            });

            if all_cancelled {
                debug!("All orders confirmed cancelled for market={market_id}");
                break;
            }

            sleep(Duration::from_millis(100)).await;
        }

        // Update local state for any still not confirmed via WS
        for id in &order_ids {
            if let Some(mut order) = state.my_orders.get_mut(id) {
                if order.status != OrderStatus::Canceled {
                    warn!("Order {id} cancel not confirmed within timeout, marking locally");
                    order.status = OrderStatus::Canceled;
                    order.updated_at = Utc::now();
                }
            }
        }

        Ok(order_ids)
    }

    /// Cancel ALL orders across all markets (L3 emergency).
    /// Acquires RiskController lock briefly for registration, then releases before HTTP.
    pub async fn cancel_all_orders(
        &mut self,
        state: &SharedState,
        risk_controller: &Arc<Mutex<RiskController>>,
    ) -> Result<()> {
        warn!("EMERGENCY: Cancelling ALL orders across all markets");

        let all_order_ids: Vec<String> = state
            .my_orders
            .iter()
            .filter(|entry| {
                matches!(entry.status, OrderStatus::Live | OrderStatus::Pending)
            })
            .map(|entry| entry.order_id.clone())
            .collect();

        // Brief lock: register cancel requests
        {
            let mut rc = risk_controller.lock().await;
            for id in &all_order_ids {
                rc.register_cancel(id.clone());
            }
        }

        // Call real API cancel-all (NO lock held during HTTP)
        if let Err(e) = self.client.cancel_all_orders().await {
            error!("API cancel-all failed: {e:#}, marking locally");
        } else {
            info!("API cancel-all succeeded for {} orders", all_order_ids.len());
        }

        // Mark all as cancelled locally regardless
        for id in &all_order_ids {
            if let Some(mut order) = state.my_orders.get_mut(id) {
                order.status = OrderStatus::Canceled;
                order.updated_at = Utc::now();
            }
        }

        Ok(())
    }

    /// Submit a batch of new orders.
    /// Splits into batches of `batch_size` (max 15 per Polymarket).
    pub async fn submit_orders(
        &mut self,
        state: &SharedState,
        orders: Vec<QuoteOrder>,
    ) -> Result<Vec<String>> {
        let mut submitted_ids = Vec::new();

        for batch in orders.chunks(self.config.batch_size) {
            match self.submit_batch(state, batch).await {
                Ok(ids) => {
                    submitted_ids.extend(ids);
                }
                Err(e) => {
                    error!("Batch submission failed: {e:#}");
                    // Continue with next batch (partial success is better than none)
                }
            }
        }

        info!(
            "Submitted {}/{} orders successfully",
            submitted_ids.len(),
            orders.len()
        );

        Ok(submitted_ids)
    }

    /// Submit a single batch of orders via SDK (max 15).
    /// Uses index-aligned Option results to correctly pair responses with input orders.
    async fn submit_batch(
        &self,
        state: &SharedState,
        batch: &[QuoteOrder],
    ) -> Result<Vec<String>> {
        // Returns Vec<Option<String>> preserving index alignment with batch
        let results = self.client.post_orders_batch(batch).await?;

        let mut order_ids = Vec::new();

        for (i, maybe_id) in results.iter().enumerate() {
            if let Some(order_id) = maybe_id {
                let order = &batch[i];
                state.my_orders.insert(
                    order_id.clone(),
                    OrderRecord {
                        order_id: order_id.clone(),
                        market_id: order.market_id.clone(),
                        price: order.price,
                        size: order.size,
                        side: order.side,
                        status: OrderStatus::Live,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    },
                );

                debug!(
                    "Order live: id={order_id}, market={}, side={}, price={}, size={}",
                    order.market_id, order.side, order.price, order.size
                );

                order_ids.push(order_id.clone());
            }
        }

        Ok(order_ids)
    }

    /// Cancel orders for a specific market token with exponential backoff retry.
    async fn cancel_market_with_retry(&self, token_id: &str) -> Result<()> {
        let mut delay = Duration::from_millis(self.config.base_retry_delay_ms);
        let max_delay = Duration::from_millis(self.config.max_retry_delay_ms);

        for attempt in 0..self.config.max_retries {
            let result = self.client.cancel_market_orders(token_id).await;

            match result {
                Ok(()) => {
                    info!("Market cancel sent for token={token_id} (attempt {attempt})");
                    return Ok(());
                }
                Err(e) => {
                    if attempt + 1 < self.config.max_retries {
                        warn!(
                            "Cancel attempt {} failed for token={token_id}: {e:#}, retrying in {delay:?}",
                            attempt + 1
                        );
                        sleep(delay).await;
                        delay = (delay * 2).min(max_delay);
                    } else {
                        return Err(e.context(format!(
                            "Failed to cancel orders for token={token_id} after {} attempts",
                            self.config.max_retries
                        )));
                    }
                }
            }
        }

        Ok(())
    }
}
