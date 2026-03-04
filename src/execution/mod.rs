use anyhow::Result;
use chrono::Utc;
use tokio::time::{Duration, sleep};
use tracing::{debug, error, info, warn};

use crate::config::{AppConfig, ExecutionConfig};
use crate::data::rest::ClobClient;
use crate::data::state::{OrderRecord, OrderStatus, SharedState};
use crate::pricing::QuoteOrder;
use crate::risk::RiskController;

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

    /// Cancel all orders for a specific market.
    /// Registers cancel requests in RiskController for ghost fill detection.
    /// Returns the set of order IDs that were cancelled.
    pub async fn cancel_market_orders(
        &mut self,
        state: &SharedState,
        market_id: &str,
        risk_controller: &mut RiskController,
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

        // Register cancel requests in RiskController (single source of truth for ghost fill detection)
        for id in &order_ids {
            risk_controller.register_cancel(id.clone());
        }

        // Call real API with retry
        self.cancel_with_retry(market_id).await?;

        // Wait for confirmation via WebSocket (with timeout)
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
    /// Registers cancel requests in RiskController for ghost fill detection.
    pub async fn cancel_all_orders(
        &mut self,
        state: &SharedState,
        risk_controller: &mut RiskController,
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

        // Register all cancels in RiskController
        for id in &all_order_ids {
            risk_controller.register_cancel(id.clone());
        }

        // Call real API cancel-all
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

    /// Submit a single batch of orders via SDK (max 15)
    async fn submit_batch(
        &self,
        state: &SharedState,
        batch: &[QuoteOrder],
    ) -> Result<Vec<String>> {
        // Use SDK batch submission
        let order_ids = self.client.post_orders_batch(batch).await?;

        // Record submitted orders in local state
        for (order, order_id) in batch.iter().zip(order_ids.iter()) {
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
        }

        Ok(order_ids)
    }

    /// Cancel market orders with exponential backoff retry.
    async fn cancel_with_retry(&self, market_id: &str) -> Result<()> {
        let mut delay = Duration::from_millis(self.config.base_retry_delay_ms);
        let max_delay = Duration::from_millis(self.config.max_retry_delay_ms);

        for attempt in 0..self.config.max_retries {
            let result = self.client.cancel_all_orders().await;

            match result {
                Ok(()) => {
                    info!("Cancel request sent for market={market_id} (attempt {attempt})");
                    return Ok(());
                }
                Err(e) => {
                    if attempt + 1 < self.config.max_retries {
                        warn!(
                            "Cancel attempt {} failed for market={market_id}: {e:#}, retrying in {delay:?}",
                            attempt + 1
                        );
                        sleep(delay).await;
                        delay = (delay * 2).min(max_delay);
                    } else {
                        return Err(e.context(format!(
                            "Failed to cancel orders for market={market_id} after {} attempts",
                            self.config.max_retries
                        )));
                    }
                }
            }
        }

        Ok(())
    }
}
