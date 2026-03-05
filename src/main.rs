use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use polymarket_mm::{config, data, execution, monitor, position, pricing, risk};

/// R6-1: Read and clear secrets while single-threaded, before tokio runtime starts.
/// This avoids the unsound `unsafe { remove_var() }` inside a multi-threaded runtime.
fn read_and_clear_secrets() -> Result<(String, String, String, String)> {
    let api_key = std::env::var("POLYMARKET_API_KEY")
        .context("POLYMARKET_API_KEY not set")?;
    let api_secret = std::env::var("POLYMARKET_API_SECRET")
        .context("POLYMARKET_API_SECRET not set")?;
    let api_passphrase = std::env::var("POLYMARKET_API_PASSPHRASE")
        .context("POLYMARKET_API_PASSPHRASE not set")?;
    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY")
        .context("POLYMARKET_PRIVATE_KEY not set")?;

    // SAFETY: Only main thread is running (tokio runtime not yet started).
    // remove_var is unsafe in Rust 2024 because concurrent env access is UB.
    unsafe {
        std::env::remove_var("POLYMARKET_PRIVATE_KEY");
        std::env::remove_var("POLYMARKET_API_SECRET");
        std::env::remove_var("POLYMARKET_API_PASSPHRASE");
        std::env::remove_var("POLYMARKET_API_KEY");
    }

    Ok((api_key, api_secret, api_passphrase, private_key))
}

fn main() -> Result<()> {
    // Install rustls crypto provider before any TLS connections are made.
    // rustls 0.23+ requires an explicit provider; without this, WS/HTTP threads panic.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls CryptoProvider");

    // TUI mode: redirect tracing to file (TUI owns the terminal)
    #[cfg(feature = "tui")]
    {
        let log_file = std::fs::File::create("bot.log")
            .context("Failed to create bot.log for TUI mode")?;
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "polymarket_mm=info,warn".into()),
            )
            .with_target(true)
            .with_thread_ids(true)
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false)
            .init();
    }

    // Headless mode: log to stderr as usual
    #[cfg(not(feature = "tui"))]
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "polymarket_mm=info,warn".into()),
        )
        .with_target(true)
        .with_thread_ids(true)
        .init();

    info!("Polymarket MM Bot v{} starting...", env!("CARGO_PKG_VERSION"));

    // Load environment variables (sync, single-threaded)
    dotenvy::dotenv().ok();

    // R6-1: Read and clear secrets while single-threaded (before tokio runtime).
    let (api_key, api_secret, api_passphrase, private_key) = read_and_clear_secrets()?;
    info!("Secrets loaded and cleared from environment");

    // Start tokio runtime — worker threads spawn here
    tokio::runtime::Runtime::new()?
        .block_on(async_main(api_key, api_secret, api_passphrase, private_key))
}

async fn async_main(
    api_key: String,
    api_secret: String,
    api_passphrase: String,
    private_key: String,
) -> Result<()> {
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

    // Step 3: Validate API connection (secrets moved in, not cloned)
    // R8-SEC1: Pass ownership instead of clone to avoid extra heap copies of secrets.
    // create_clob_client zeroizes consumed strings internally after SDK consumption.
    let clob_client = data::rest::create_clob_client(
        &app_config,
        api_key,
        api_secret,
        api_passphrase,
        private_key,
    ).await?;
    info!("CLOB API connection validated, secrets zeroized");

    // Step 4: Initialize risk controller (shared via Arc<Mutex> for WS access)
    let risk_controller = Arc::new(Mutex::new(risk::RiskController::new(&app_config.risk)));
    info!("Risk controller initialized at L1");

    // Step 5: Initialize execution layer
    let executor = execution::OrderExecutor::new(clob_client, &app_config);
    info!("Order executor initialized");

    // Step 6: Initialize pricing engine
    let pricing_engine = pricing::PricingEngine::new(&app_config.pricing, &app_config.risk);
    info!("Pricing engine initialized");

    // Step 7: Initialize position manager
    let position_manager = position::PositionManager::new(&app_config.position);
    info!("Position manager initialized");

    // Step 8: Initialize CTF merger for on-chain merge operations (optional)
    let ctf_merger = match data::ctf::CtfMerger::new(
        &app_config.api.polygon_rpc_url,
        executor.client().clone_signer(),
    ).await {
        Ok(merger) => {
            info!("CTF merger initialized for on-chain merge operations");
            Some(merger)
        }
        Err(e) => {
            warn!("CTF merger initialization failed: {e:#}. Merge operations will be skipped.");
            None
        }
    };

    // Step 9: Set up TUI channels (if feature enabled)
    #[cfg(feature = "tui")]
    let tui_channels = {
        let (snapshot_tx, snapshot_rx) = tokio::sync::mpsc::channel(4);
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(4);

        // Spawn TUI task
        info!("Starting TUI dashboard...");
        tokio::spawn(async move {
            if let Err(e) = polymarket_mm::tui::run_tui(snapshot_rx, cmd_tx).await {
                error!("TUI task error: {e}");
            }
        });

        Some((snapshot_tx, cmd_rx))
    };

    // Step 10: Run main orchestration loop
    info!("Starting main loop...");
    let result = monitor::run_orchestrator(
        app_config,
        state,
        risk_controller,
        executor,
        pricing_engine,
        position_manager,
        ctf_merger,
        #[cfg(feature = "tui")]
        tui_channels,
    )
    .await;

    match &result {
        Ok(()) => info!("Bot stopped gracefully"),
        Err(e) => error!("Bot stopped with error: {e:#}"),
    }

    result
}
