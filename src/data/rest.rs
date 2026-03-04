use std::str::FromStr;

use alloy::primitives::U256;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer as _;
use anyhow::{Context, Result};
use rust_decimal::Decimal;
use tracing::{debug, info, warn};
use uuid::Uuid;

use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::{Credentials, Normal};
use polymarket_client_sdk::clob::types::request::{
    BalanceAllowanceRequest, CancelMarketOrderRequest, OrdersRequest,
};
use polymarket_client_sdk::clob::types::{AssetType, OrderType, Side};
use polymarket_client_sdk::POLYGON;

use crate::config::AppConfig;
use crate::data::state::OrderSide;
use crate::pricing::QuoteOrder;

/// Authenticated CLOB client wrapping `polymarket-client-sdk`.
#[allow(dead_code)]
pub struct ClobClient {
    /// Authenticated SDK client (Normal L2 auth)
    sdk: polymarket_client_sdk::clob::Client<Authenticated<Normal>>,
    /// Signer for order signing (needed by `sdk.sign()`)
    signer: PrivateKeySigner,
    /// API credentials for WebSocket authentication
    pub credentials: Credentials,
    /// Wallet address
    pub address: alloy::primitives::Address,
    /// Base URL for reference
    pub base_url: String,
}

impl ClobClient {
    /// Verify API connectivity by fetching USDC balance
    pub async fn check_connection(&self) -> Result<Decimal> {
        let balance = self.fetch_collateral_balance().await?;
        info!("CLOB API authenticated, USDC balance: ${balance}");
        Ok(balance)
    }

    /// Submit a single limit order via the SDK.
    /// Returns the order_id assigned by the exchange.
    #[allow(dead_code)]
    pub async fn post_limit_order(
        &self,
        token_id: &str,
        side: OrderSide,
        price: Decimal,
        size: Decimal,
    ) -> Result<String> {
        let token_id = U256::from_str(token_id)
            .context("Invalid token_id format")?;

        let sdk_side = match side {
            OrderSide::Buy => Side::Buy,
            OrderSide::Sell => Side::Sell,
        };

        let signable_order = self.sdk
            .limit_order()
            .token_id(token_id)
            .side(sdk_side)
            .price(price)
            .size(size)
            .order_type(OrderType::GTC)
            .build()
            .await
            .context("Failed to build limit order")?;

        let signed_order = self.sdk
            .sign(&self.signer, signable_order)
            .await
            .context("Failed to sign order")?;

        let response = self.sdk
            .post_order(signed_order)
            .await
            .context("Failed to post order")?;

        if !response.success {
            let err_msg = response.error_msg.unwrap_or_default();
            anyhow::bail!("Order rejected: {err_msg}");
        }

        debug!(
            "Order placed: id={}, side={:?}, price={}, size={}",
            response.order_id, side, price, size
        );

        Ok(response.order_id)
    }

    /// Submit a batch of orders.
    /// Returns Vec<Option<String>> preserving index alignment with input:
    /// Some(order_id) for accepted, None for rejected.
    pub async fn post_orders_batch(
        &self,
        orders: &[QuoteOrder],
    ) -> Result<Vec<Option<String>>> {
        let mut signed_orders = Vec::with_capacity(orders.len());

        for order in orders {
            let token_id = U256::from_str(&order.token_id)
                .context("Invalid token_id format")?;

            let sdk_side = match order.side {
                OrderSide::Buy => Side::Buy,
                OrderSide::Sell => Side::Sell,
            };

            let signable = self.sdk
                .limit_order()
                .token_id(token_id)
                .side(sdk_side)
                .price(order.price)
                .size(order.size)
                .order_type(OrderType::GTC)
                .build()
                .await
                .context("Failed to build order")?;

            let signed = self.sdk
                .sign(&self.signer, signable)
                .await
                .context("Failed to sign order")?;

            signed_orders.push(signed);
        }

        let responses = self.sdk
            .post_orders(signed_orders)
            .await
            .context("Failed to post orders batch")?;

        let mut results = Vec::with_capacity(responses.len());
        for (i, resp) in responses.iter().enumerate() {
            if resp.success {
                results.push(Some(resp.order_id.clone()));
            } else {
                let err = resp.error_msg.as_deref().unwrap_or("unknown");
                warn!("Order {i} in batch rejected: {err}");
                results.push(None);
            }
        }

        Ok(results)
    }

    /// Cancel a single order by ID
    #[allow(dead_code)]
    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        let resp = self.sdk
            .cancel_order(order_id)
            .await
            .context("Failed to cancel order")?;
        debug!("Order cancelled: {order_id}, canceled={:?}", resp.canceled);
        Ok(())
    }

    /// Cancel ALL open orders
    pub async fn cancel_all_orders(&self) -> Result<()> {
        let resp = self.sdk
            .cancel_all_orders()
            .await
            .context("Failed to cancel all orders")?;
        info!("All orders cancelled via API: canceled={}", resp.canceled.len());
        Ok(())
    }

    /// Fetch all open orders (paginated with safety limit). Returns a flat list.
    pub async fn fetch_open_orders(&self) -> Result<Vec<polymarket_client_sdk::clob::types::response::OpenOrderResponse>> {
        const MAX_PAGES: u32 = 10;
        let mut all_orders = Vec::new();
        let mut cursor = None;
        let request = OrdersRequest::builder().build();

        for page_num in 1..=MAX_PAGES {
            let page = self.sdk.orders(&request, cursor).await
                .context("Failed to fetch open orders")?;

            all_orders.extend(page.data);

            if page.next_cursor.is_empty() || page.next_cursor == "LTE=" {
                break;
            }
            if page_num == MAX_PAGES {
                warn!(
                    "fetch_open_orders hit pagination limit ({MAX_PAGES} pages), {} orders loaded so far",
                    all_orders.len()
                );
                break;
            }
            cursor = Some(page.next_cursor);
        }

        info!("Fetched {} open orders from API", all_orders.len());
        Ok(all_orders)
    }

    /// Get USDC balance for collateral
    pub async fn fetch_collateral_balance(&self) -> Result<Decimal> {
        let req = BalanceAllowanceRequest::builder()
            .asset_type(AssetType::Collateral)
            .build();

        let resp = self.sdk.balance_allowance(req).await
            .context("Failed to fetch collateral balance")?;

        info!("USDC balance: {}", resp.balance);
        Ok(resp.balance)
    }

    /// Get conditional token balance for a specific token_id
    pub async fn fetch_token_balance(&self, token_id: &str) -> Result<Decimal> {
        let asset_id = U256::from_str(token_id)
            .context("Invalid token_id")?;

        let req = BalanceAllowanceRequest::builder()
            .asset_type(AssetType::Conditional)
            .token_id(asset_id)
            .build();

        let resp = self.sdk.balance_allowance(req).await
            .context("Failed to fetch token balance")?;

        debug!("Token balance for {token_id}: {}", resp.balance);
        Ok(resp.balance)
    }

    /// Cancel all orders for a specific market/token
    pub async fn cancel_market_orders(&self, token_id: &str) -> Result<()> {
        let asset_id = U256::from_str(token_id)
            .context("Invalid token_id for cancel")?;

        let req = CancelMarketOrderRequest::builder()
            .asset_id(asset_id)
            .build();

        let resp = self.sdk
            .cancel_market_orders(&req)
            .await
            .context("Failed to cancel market orders")?;

        debug!(
            "Market orders cancelled for token={token_id}, canceled={}",
            resp.canceled.len()
        );
        Ok(())
    }
}

/// Create and authenticate a CLOB client from environment variables
pub async fn create_clob_client(config: &AppConfig) -> Result<ClobClient> {
    let api_key_str = std::env::var("POLYMARKET_API_KEY")
        .context("POLYMARKET_API_KEY not set")?;
    let api_secret = std::env::var("POLYMARKET_API_SECRET")
        .context("POLYMARKET_API_SECRET not set")?;
    let api_passphrase = std::env::var("POLYMARKET_API_PASSPHRASE")
        .context("POLYMARKET_API_PASSPHRASE not set")?;
    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
        .context("POLYMARKET_PRIVATE_KEY not set")?;

    info!("Initializing CLOB client at {}", config.api.clob_base_url);

    // Create signer from private key, set chain to Polygon mainnet
    let signer: PrivateKeySigner = private_key.parse()
        .context("Invalid private key format")?;
    let signer = signer.with_chain_id(Some(POLYGON));

    let address = signer.address();
    info!("Wallet address: {address}");

    // Authenticate with SDK — builds an Authenticated<Normal> client
    let sdk = polymarket_client_sdk::clob::Client::new(
        &config.api.clob_base_url,
        polymarket_client_sdk::clob::Config::default(),
    )
    .context("Failed to create CLOB client")?
    .authentication_builder(&signer)
    .authenticate()
    .await
    .context("Failed to authenticate with Polymarket CLOB API")?;

    // Parse API key as UUID for credentials
    let api_key = Uuid::parse_str(&api_key_str)
        .context("POLYMARKET_API_KEY must be a valid UUID")?;

    let credentials = Credentials::new(api_key, api_secret, api_passphrase);

    let client = ClobClient {
        sdk,
        signer,
        credentials,
        address,
        base_url: config.api.clob_base_url.clone(),
    };

    client.check_connection().await?;
    Ok(client)
}
