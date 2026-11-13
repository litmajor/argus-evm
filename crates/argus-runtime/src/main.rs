//! argus-evm — High-frequency EVM liquidation & oracle-lag arbitrage runtime.
//!
//! SSA Runtime layer: wires Ingestor → State → Sim → Executor as concurrent
//! Tokio tasks communicating via broadcast + mpsc channels.

mod discovery;

use std::{path::Path, path::PathBuf, str::FromStr};

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{bail, Context};
use argus_executor::{ExecutorConfig, SubmitMode};
use argus_ingestor::IngestorConfig;
use argus_sim::SimConfig;
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_MORPHO_ADDRESS: &str = "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb";

#[derive(Debug, Parser)]
#[command(name = "argus-evm", about = "EVM liquidation runtime (SSA)")]
struct Cli {
    /// WebSocket RPC URL
    #[arg(long, env = "ARGUS_WS_URL", default_value = "wss://eth.llamarpc.com")]
    ws_url: String,

    /// HTTP RPC URL; defaults to the HTTP equivalent of ARGUS_WS_URL.
    #[arg(long, env = "ARGUS_RPC_URL")]
    rpc_url: Option<String>,

    /// Chain id
    #[arg(long, env = "ARGUS_CHAIN_ID", default_value = "1")]
    chain_id: u64,

    /// Morpho Blue singleton address
    #[arg(long, env = "ARGUS_MORPHO_ADDRESS", default_value = DEFAULT_MORPHO_ADDRESS)]
    morpho_address: String,

    /// First block to scan for historical Morpho Borrow events.
    #[arg(long, env = "ARGUS_MORPHO_SCAN_FROM_BLOCK", default_value_t = 0)]
    morpho_scan_from_block: u64,

    /// Deployed VaultLiquidator contract address
    #[arg(long, env = "ARGUS_LIQUIDATOR_ADDRESS")]
    liquidator_address: String,

    /// Explicit simulation sender; otherwise derived from ARGUS_PRIVATE_KEY.
    #[arg(long, env = "ARGUS_SIM_CALLER_ADDRESS")]
    sim_caller_address: Option<String>,

    /// Path to JSON config (optional; reserved for future address configuration)
    #[arg(long, env = "ARGUS_CONFIG")]
    config: Option<PathBuf>,

    /// JSONL telemetry output path
    #[arg(
        long,
        env = "ARGUS_TELEMETRY",
        default_value = "telemetry/argus-evm.jsonl"
    )]
    telemetry: String,

    /// Log level
    #[arg(long, env = "RUST_LOG", default_value = "info")]
    log_level: String,

    /// Executor private key (hex). Prefer env ARGUS_PRIVATE_KEY.
    #[arg(long, env = "ARGUS_PRIVATE_KEY", default_value = "")]
    private_key: String,

    /// Private RPC / builder URL
    #[arg(
        long,
        env = "ARGUS_PRIVATE_RPC",
        default_value = "https://relay.flashbots.net"
    )]
    private_rpc: String,

    /// Submission mode: flashbots_bundle, private_tx, or private_rpc.
    #[arg(long, env = "ARGUS_SUBMIT_MODE")]
    submit_mode: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv()?;
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .with_target(true)
        .with_thread_ids(false)
        .json()
        .init();

    let liquidator = parse_address("ARGUS_LIQUIDATOR_ADDRESS", &cli.liquidator_address)?;
    let morpho_address = parse_address("ARGUS_MORPHO_ADDRESS", &cli.morpho_address)?;
    anyhow::ensure!(
        !liquidator.is_zero(),
        "ARGUS_LIQUIDATOR_ADDRESS cannot be zero"
    );
    anyhow::ensure!(
        !morpho_address.is_zero(),
        "ARGUS_MORPHO_ADDRESS cannot be zero"
    );
    let caller = resolve_sim_caller(cli.sim_caller_address.as_deref(), &cli.private_key)?;
    let http_rpc_url = cli
        .rpc_url
        .clone()
        .unwrap_or_else(|| http_url_from_ws(&cli.ws_url));
    let submit_mode = resolve_submit_mode(cli.submit_mode.as_deref(), &cli.private_rpc)?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        chain_id = cli.chain_id,
        %liquidator,
        %morpho_address,
        %caller,
        scan_from_block = cli.morpho_scan_from_block,
        "argus-evm starting (SSA runtime)"
    );
    if cli.morpho_scan_from_block == 0 {
        warn!("scanning Morpho Borrow history from genesis may take a long time; set ARGUS_MORPHO_SCAN_FROM_BLOCK to a suitable deployment block");
    }

    if let Some(config_path) = &cli.config {
        warn!(path = %config_path.display(), "ARGUS_CONFIG is not loaded by this runtime");
    }

    let ingestor_cfg = IngestorConfig {
        ws_url: cli.ws_url.clone(),
        morpho_blue_addresses: vec![morpho_address],
        euler_evc_addresses: vec![],
        chain_id: cli.chain_id,
    };

    let sim_cfg = SimConfig {
        liquidator,
        caller,
        morpho_blue: morpho_address,
        chain_id: cli.chain_id,
        http_rpc_url: http_rpc_url.clone(),
        ..SimConfig::default()
    };

    let exec_cfg = ExecutorConfig {
        chain_id: cli.chain_id,
        telemetry_path: cli.telemetry.clone(),
        private_key_hex: cli.private_key.clone(),
        private_rpc_url: cli.private_rpc.clone(),
        public_rpc_url: Some(http_rpc_url.clone()),
        submit_mode,
        ..ExecutorConfig::default()
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (ingestor, event_rx_for_state) = argus_ingestor::Ingestor::new(ingestor_cfg);
    let _event_rx_extra = ingestor.subscribe();
    let (candidate_tx, candidate_rx) =
        tokio::sync::mpsc::channel(argus_state::CANDIDATE_CHANNEL_CAPACITY);

    let ingestor_handle = ingestor.spawn(shutdown_rx.clone());
    let state_handle = argus_state::spawn_with_candidate_sender(
        event_rx_for_state,
        shutdown_rx.clone(),
        candidate_tx.clone(),
    );
    let (sim_rx, sim_handle) = argus_sim::spawn(candidate_rx, sim_cfg, shutdown_rx.clone());
    let exec_handle = argus_executor::spawn(sim_rx, exec_cfg, shutdown_rx.clone());

    let discovered = match discovery::discover_candidates(
        &http_rpc_url,
        morpho_address,
        cli.morpho_scan_from_block,
    )
    .await
    {
        Ok(candidates) => candidates,
        Err(error) => {
            let _ = shutdown_tx.send(true);
            let _ = tokio::join!(ingestor_handle, state_handle, sim_handle, exec_handle);
            return Err(error).context("historical Morpho candidate discovery failed");
        }
    };
    for candidate in discovered {
        candidate_tx
            .send(candidate)
            .await
            .context("failed to send discovered candidate to simulator")?;
    }
    drop(candidate_tx);

    info!("all SSA surfaces spawned; waiting for shutdown signal");
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for ctrl-c")?;
    info!("ctrl-c received; shutting down");
    let _ = shutdown_tx.send(true);
    let _ = tokio::join!(ingestor_handle, state_handle, sim_handle, exec_handle);
    info!("argus-evm stopped");
    Ok(())
}

fn load_dotenv() -> anyhow::Result<()> {
    let path = Path::new(".env");
    if path.is_file() {
        dotenvy::from_path(path).context("failed to load .env")?;
    }
    Ok(())
}

fn parse_address(name: &str, value: &str) -> anyhow::Result<Address> {
    Address::from_str(value).with_context(|| format!("{name} must be a valid EVM address"))
}

fn resolve_sim_caller(
    configured_address: Option<&str>,
    private_key: &str,
) -> anyhow::Result<Address> {
    if let Some(address) = configured_address {
        let address = parse_address("ARGUS_SIM_CALLER_ADDRESS", address)?;
        anyhow::ensure!(
            !address.is_zero(),
            "ARGUS_SIM_CALLER_ADDRESS cannot be zero"
        );
        return Ok(address);
    }

    if !private_key.is_empty() {
        let signer = private_key
            .parse::<PrivateKeySigner>()
            .context("ARGUS_PRIVATE_KEY is invalid; set ARGUS_SIM_CALLER_ADDRESS to override the simulation sender")?;
        return Ok(signer.address());
    }

    bail!("set ARGUS_SIM_CALLER_ADDRESS or ARGUS_PRIVATE_KEY so simulation uses the contract owner/operator")
}

fn http_url_from_ws(ws_url: &str) -> String {
    ws_url
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1)
}

fn resolve_submit_mode(mode: Option<&str>, private_rpc: &str) -> anyhow::Result<SubmitMode> {
    let mode = mode
        .map(str::to_ascii_lowercase)
        .or_else(|| is_loopback_url(private_rpc).then_some("private_rpc".to_string()));

    match mode.as_deref() {
        Some("flashbots_bundle") => Ok(SubmitMode::FlashbotsBundle),
        Some("private_tx") => Ok(SubmitMode::PrivateTx),
        Some("private_rpc") => Ok(SubmitMode::PrivateRpc),
        Some(other) => bail!(
            "unsupported ARGUS_SUBMIT_MODE {other:?}; use flashbots_bundle, private_tx, or private_rpc"
        ),
        None => Ok(SubmitMode::default()),
    }
}

fn is_loopback_url(url: &str) -> bool {
    url.contains("://127.0.0.1:") || url.contains("://localhost:") || url.contains("://[::1]:")
}
