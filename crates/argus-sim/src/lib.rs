//! # argus-sim — SSA Simulation Surface
//!
//! Embeds `revm` in-process to simulate the atomic Morpho callback path:
//!   liquidate → swap seized collateral → repay Morpho
//! against a cloned fork of the latest block state.
//!
//! Target: < 2 ms per candidate on hot path.

pub mod alloy_db;
pub mod config;
pub mod fork;
pub mod morpho_calldata;
pub mod simulator;

use argus_types::{LiquidationCandidate, Result, SimulationResult};
use tokio::sync::{mpsc, watch};
use tracing::info;

pub use config::SimConfig;
pub use simulator::Simulator;

pub const SIM_RESULT_CHANNEL_CAPACITY: usize = 512;

/// Spawn the simulation worker pool.
///
/// Consumes `LiquidationCandidate`s and produces `SimulationResult`s.
pub fn spawn(
    mut candidate_rx: mpsc::Receiver<LiquidationCandidate>,
    config: SimConfig,
    shutdown: watch::Receiver<bool>,
) -> (
    mpsc::Receiver<SimulationResult>,
    tokio::task::JoinHandle<Result<()>>,
) {
    let (result_tx, result_rx) = mpsc::channel(SIM_RESULT_CHANNEL_CAPACITY);

    let handle = tokio::spawn(async move {
        info!("argus-sim starting");
        let sim = Simulator::new(config);

        loop {
            tokio::select! {
                _ = async {
                    let mut s = shutdown.clone();
                    let _ = s.changed().await;
                    while !*s.borrow() {
                        let _ = s.changed().await;
                    }
                } => {
                    info!("argus-sim shutting down");
                    break;
                }
                candidate = candidate_rx.recv() => {
                    match candidate {
                        Some(c) => {
                            match sim.simulate(&c).await {
                                Ok(result) => {
                                    if result_tx.send(result).await.is_err() {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "simulation error");
                                }
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        Ok(())
    });

    (result_rx, handle)
}
