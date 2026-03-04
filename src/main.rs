mod config;
mod data;
mod execution;
mod monitor;
mod position;
mod pricing;
mod risk;

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "polymarket_mm=info,warn".into()),
        )
        .with_target(true)
        .with_thread_ids(true)
        .init();

    info!("Polymarket MM Bot v{} starting...", env!("CARGO_PKG_VERSION"));

    // Load environment variables
    dotenvy::dotenv().ok();

    // Step 1: Load configuration
    let app_config = config::AppConfig::load()?;
    info!(
        "Config loaded: {} markets, capital ${:.2}",
        app_config.markets.len(),
        app_config.capital.total_capital
    );

    // Step 2: Initialize shared state
    let state = data::SharedState::new(&app_config);
    info!("Shared state initialized");

    // Step 3: Validate API connection
    let clob_client = data::rest::create_clob_client(&app_config).await?;
    info!("CLOB API connection validated");

    // Step 4: Initialize risk controller (shared via Arc<Mutex> for WS access)
    let risk_controller = Arc::new(Mutex::new(risk::RiskController::new(&app_config.risk)));
    info!("Risk controller initialized at L1");

    // Step 5: Initialize execution layer
    let executor = execution::OrderExecutor::new(clob_client, &app_config);
    info!("Order executor initialized");

    // Step 6: Initialize pricing engine
    let pricing_engine = pricing::PricingEngine::new(&app_config.pricing);
    info!("Pricing engine initialized");

    // Step 7: Initialize position manager
    let position_manager = position::PositionManager::new(&app_config.position);
    info!("Position manager initialized");

    // Step 8: Run main orchestration loop
    info!("Starting main loop...");
    let result = monitor::run_orchestrator(
        app_config,
        state,
        risk_controller,
        executor,
        pricing_engine,
        position_manager,
    )
    .await;

    match &result {
        Ok(()) => info!("Bot stopped gracefully"),
        Err(e) => error!("Bot stopped with error: {e:#}"),
    }

    result
}
