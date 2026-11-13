//! # argus-state — SSA Domain Engine
//!
//! Maintains an in-memory hot mirror of markets, positions, and oracle prices.
//! Listens to `DomainEvent`s from the ingestor, updates local state without
//! blocking RPC, evaluates health factors, and emits `LiquidationCandidate`s
//! downstream via an mpsc channel when HF < 1.0.

pub mod engine;
pub mod health;
pub mod store;

use std::sync::Arc;

use argus_types::{DomainEvent, LiquidationCandidate, Result};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

pub use engine::StateEngine;
pub use store::StateStore;

/// Capacity of the candidate channel (bounded to apply back-pressure).
pub const CANDIDATE_CHANNEL_CAPACITY: usize = 1024;

/// Spawn the state engine as a Tokio task.
///
/// - `event_rx`: broadcast receiver from the ingestor
/// - returns `(candidate_rx, join_handle)`
pub fn spawn(
    event_rx: broadcast::Receiver<DomainEvent>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> (
    mpsc::Receiver<LiquidationCandidate>,
    tokio::task::JoinHandle<Result<()>>,
) {
    let (candidate_tx, candidate_rx) = mpsc::channel(CANDIDATE_CHANNEL_CAPACITY);
    let handle = spawn_with_candidate_sender(event_rx, shutdown, candidate_tx);
    (candidate_rx, handle)
}

/// Spawn the state engine using a caller-provided candidate channel.
///
/// This lets startup discovery feed candidates into the same simulation
/// pipeline as live state events.
pub fn spawn_with_candidate_sender(
    event_rx: broadcast::Receiver<DomainEvent>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    candidate_tx: mpsc::Sender<LiquidationCandidate>,
) -> tokio::task::JoinHandle<Result<()>> {
    let store = Arc::new(StateStore::new());
    let engine = StateEngine::new(store, candidate_tx);

    tokio::spawn(async move {
        info!("argus-state engine starting");
        engine.run(event_rx, shutdown).await
    })
}
