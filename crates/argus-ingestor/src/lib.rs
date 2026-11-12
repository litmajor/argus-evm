//! # argus-ingestor — SSA Collector Layer
//!
//! Connects to an EVM WebSocket provider via Alloy, subscribes to `newHeads`
//! and protocol log filters (Morpho Blue + Euler v2 EVC), normalizes them into
//! `DomainEvent`s, and fans them out over a Tokio `broadcast` channel.

pub mod config;
pub mod filters;
pub mod morpho;
pub mod euler;
pub mod provider;

use alloy_primitives::B256;
use argus_types::{
    ArgusError, BlockHeader, DomainEvent, EventId, NewHeadEvent, Result,
};
use chrono::Utc;
use futures::StreamExt;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

pub use config::IngestorConfig;
pub use provider::AlloyProviderHandle;

/// Capacity of the domain-event broadcast channel.
pub const EVENT_CHANNEL_CAPACITY: usize = 4096;

/// The Ingestor surface. Owns the WS connection and emits DomainEvents.
pub struct Ingestor {
    config: IngestorConfig,
    event_tx: broadcast::Sender<DomainEvent>,
}

impl Ingestor {
    pub fn new(config: IngestorConfig) -> (Self, broadcast::Receiver<DomainEvent>) {
        let (event_tx, event_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        (Self { config, event_tx }, event_rx)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.event_tx.subscribe()
    }

    pub fn sender(&self) -> broadcast::Sender<DomainEvent> {
        self.event_tx.clone()
    }

    /// Spawn the full ingestion pipeline as a Tokio task.
    pub fn spawn(
        self,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<Result<()>> {
        tokio::spawn(async move {
            info!(ws_url = %self.config.ws_url, "argus-ingestor starting");

            loop {
                tokio::select! {
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            info!("ingestor received shutdown");
                            break;
                        }
                    }
                    result = self.run_once() => {
                        match result {
                            Ok(()) => warn!("ingestor stream ended cleanly; reconnecting"),
                            Err(e) => {
                                error!(error = %e, "ingestor error; reconnecting in 2s");
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            }
                        }
                    }
                }
            }
            Ok(())
        })
    }

    async fn run_once(&self) -> Result<()> {
        let provider = provider::connect(&self.config.ws_url).await?;

        let heads_tx = self.event_tx.clone();
        let logs_tx = self.event_tx.clone();
        let morpho_addrs = self.config.morpho_blue_addresses.clone();
        let euler_addrs = self.config.euler_evc_addresses.clone();
        let provider_logs = provider.clone();

        let heads_handle = tokio::spawn(async move {
            stream_new_heads(provider, heads_tx).await
        });

        let logs_handle = tokio::spawn(async move {
            stream_protocol_logs(provider_logs, morpho_addrs, euler_addrs, logs_tx).await
        });

        tokio::select! {
            r = heads_handle => r.map_err(|e| ArgusError::Internal(e.to_string()))?,
            r = logs_handle => r.map_err(|e| ArgusError::Internal(e.to_string()))?,
        }
    }
}

async fn stream_new_heads(
    provider: AlloyProviderHandle,
    tx: broadcast::Sender<DomainEvent>,
) -> Result<()> {
    let mut stream = provider.subscribe_blocks().await?;

    while let Some(header) = stream.next().await {
        let block_header = BlockHeader {
            number: header.number,
            hash: header.hash,
            parent_hash: header.parent_hash,
            timestamp: header.timestamp,
            base_fee_per_gas: header.base_fee_per_gas.map(Into::into),
        };

        let evt = DomainEvent::NewHead(NewHeadEvent {
            id: EventId::new(),
            header: block_header,
            received_at: Utc::now(),
        });

        // No active receivers is not fatal — keep streaming.
        let _ = tx.send(evt);
    }

    Err(ArgusError::SubscriptionClosed)
}

async fn stream_protocol_logs(
    provider: AlloyProviderHandle,
    morpho_addrs: Vec<alloy_primitives::Address>,
    euler_addrs: Vec<alloy_primitives::Address>,
    tx: broadcast::Sender<DomainEvent>,
) -> Result<()> {
    let filter = filters::build_protocol_filter(&morpho_addrs, &euler_addrs);
    let mut stream = provider.subscribe_logs(&filter).await?;

    while let Some(log) = stream.next().await {
        if let Some(evt) = normalize_log(&log, &morpho_addrs, &euler_addrs) {
            let _ = tx.send(evt);
        }
    }

    Err(ArgusError::SubscriptionClosed)
}

fn normalize_log(
    log: &alloy_rpc_types::Log,
    morpho_addrs: &[alloy_primitives::Address],
    euler_addrs: &[alloy_primitives::Address],
) -> Option<DomainEvent> {
    let addr = log.address();
    if morpho_addrs.contains(&addr) {
        return morpho::decode_log(log);
    }
    if euler_addrs.contains(&addr) {
        return euler::decode_log(log);
    }
    Some(DomainEvent::RawLog(argus_types::RawLogEvent {
        id: EventId::new(),
        address: addr,
        topics: log.topics().to_vec(),
        data: log.data().data.to_vec(),
        block_number: log.block_number.unwrap_or(0),
        tx_hash: log.transaction_hash.unwrap_or(B256::ZERO),
        log_index: log.log_index.unwrap_or(0),
        received_at: Utc::now(),
    }))
}
