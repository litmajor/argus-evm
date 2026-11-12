//! argus-evm — High-frequency EVM liquidation & oracle-lag arbitrage runtime.
//!
//! SSA Runtime layer: wires Ingestor → State → Sim → Executor as concurrent
//! Tokio tasks communicating via broadcast + mpsc channels.

use std::path::PathBuf;

use anyhow::Context;
use argus_executor::ExecutorConfig;
use argus_ingestor::IngestorConfig;
use argus_sim::SimConfig;
use clap::Parser;
use tracing::{info, Level};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "argus-evm", about = "EVM liquidation runtime (SSA)")]
struct Cli {
    /// WebSocket RPC URL
    #[arg(long, env = "ARGUS_WS_URL", default_value = "wss://eth.llamarpc.com")]
    ws_url: String,

    /// Chain id
    #[arg(long, env = "ARGUS_CHAIN_ID", default_value = "1")]
    chain_id: u64,

    /// Path to JSON config (optional; overrides CLI for addresses etc.)
    #[arg(long, env = "ARGUS_CONFIG")]
    config: Option<PathBuf>,

    /// JSONL telemetry output path
    #[arg(long, env = "ARGUS_TELEMETRY", default_value = "telemetry/argus-evm.jsonl")]
    telemetry: String,

    /// Log level
    #[arg(long, env = "RUST_LOG", default_value = "info")]
    log_level: String,

    /// Executor private key (hex). Prefer env ARGUS_PRIVATE_KEY.
    #[arg(long, env = "ARGUS_PRIVATE_KEY", default_value = "")]
    private_key: String,

    /// Private RPC / builder URL
    #[arg(long, env = "ARGUS_PRIVATE_RPC", default_value = "https://relay.flashbots.net")]
    private_rpc: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .with_target(true)
        .with_thread_ids(false)
        .json()
        .init();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        chain_id = cli.chain_id,
        "argus-evm starting (SSA runtime)"
    );

    // ── Config ────────────────────────────────────────────────
    let ingestor_cfg = IngestorConfig {
        ws_url: cli.ws_url.clone(),
        morpho_blue_addresses: vec![], // load from config file / env in prod
        euler_evc_addresses: vec![],
        chain_id: cli.chain_id,
    };

    let sim_cfg = SimConfig {
        chain_id: cli.chain_id,
        // Derive HTTP from WS url when possible (wss→https) for the fork DB.
        http_rpc_url: cli.ws_url
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1),
        ..SimConfig::default()
    };

    let exec_cfg = ExecutorConfig {
        chain_id: cli.chain_id,
        telemetry_path: cli.telemetry.clone(),
        private_key_hex: cli.private_key.clone(),
        private_rpc_url: cli.private_rpc.clone(),
        public_rpc_url: Some(
            cli.ws_url
                .replacen("wss://", "https://", 1)
                .replacen("ws://", "http://", 1),
        ),
        ..ExecutorConfig::default()
    };

    // ── Shutdown signal ───────────────────────────────────────
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // ── Wire the pipeline ─────────────────────────────────────
    // Ingestor (broadcast DomainEvent)
    let (ingestor, event_rx_for_state) = argus_ingestor::Ingestor::new(ingestor_cfg);
    let _event_rx_extra = ingestor.subscribe(); // available for future surfaces

    let ingestor_handle = ingestor.spawn(shutdown_rx.clone());

    // State engine (broadcast → mpsc LiquidationCandidate)
    let (candidate_rx, state_handle) =
        argus_state::spawn(event_rx_for_state, shutdown_rx.clone());

    // Simulator (mpsc candidate → mpsc SimulationResult)
    let (sim_rx, sim_handle) =
        argus_sim::spawn(candidate_rx, sim_cfg, shutdown_rx.clone());

    // Executor (mpsc SimulationResult → private RPC + JSONL)
    let exec_handle = argus_executor::spawn(sim_rx, exec_cfg, shutdown_rx);

    info!("all SSA surfaces spawned; waiting for shutdown signal");

    // ── Ctrl-C ────────────────────────────────────────────────
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for ctrl-c")?;
    info!("ctrl-c received; shutting down");
    let _ = shutdown_tx.send(true);

    // Join (best-effort)
    let _ = tokio::join!(ingestor_handle, state_handle, sim_handle, exec_handle);

    info!("argus-evm stopped");
    Ok(())
}
