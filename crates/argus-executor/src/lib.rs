//! # argus-executor — SSA Action Surface
//!
//! Takes profitable `SimulationResult`s, builds signed bundles, and submits
//! them to private RPC / Flashbots-style builders. Emits structured JSONL
//! telemetry for every attempt.

pub mod config;
pub mod submitter;
pub mod telemetry;
pub mod receipt;

use argus_types::{Result, SimulationResult};
use tokio::sync::{mpsc, watch};
use tracing::info;

pub use config::ExecutorConfig;
pub use submitter::BundleSubmitter;
pub use telemetry::JsonlTelemetry;

/// Spawn the executor task.
pub fn spawn(
    mut sim_rx: mpsc::Receiver<SimulationResult>,
    config: ExecutorConfig,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        info!("argus-executor starting");
        let telemetry = JsonlTelemetry::open(&config.telemetry_path)?;
        let wait_receipt = config.wait_for_receipt;
        let poller = receipt::ReceiptPoller::new(&config);
        let submitter = BundleSubmitter::new(config);

        loop {
            tokio::select! {
                _ = async {
                    let mut s = shutdown.clone();
                    let _ = s.changed().await;
                    while !*s.borrow() {
                        let _ = s.changed().await;
                    }
                } => {
                    info!("argus-executor shutting down");
                    break;
                }
                result = sim_rx.recv() => {
                    match result {
                        Some(sim) => {
                            if !sim.is_profitable() {
                                telemetry.log_sim_skip(&sim);
                                continue;
                            }
                            match submitter.submit(&sim).await {
                                Ok(mut outcome) => {
                                    if wait_receipt && outcome.tx_hash.is_some() {
                                        match poller.wait_for_receipt(outcome).await {
                                            Ok(confirmed) => outcome = confirmed,
                                            Err(e) => {
                                                tracing::warn!(error = %e, "receipt poll failed");
                                            }
                                        }
                                    }
                                    telemetry.log_execution(&outcome, &sim);
                                }
                                Err(e) => {
                                    telemetry.log_error("executor", &e.to_string());
                                    tracing::warn!(error = %e, "submit failed");
                                }
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        Ok(())
    })
}
