//! Alloy WebSocket provider — real `subscribe_blocks` / `subscribe_logs`.
//!
//! API matches alloy 0.9+ / current docs:
//!   ProviderBuilder::new().connect_ws(WsConnect::new(url))
//!   provider.subscribe_blocks().await?.into_stream()
//!   provider.subscribe_logs(&filter).await?.into_stream()

use std::sync::Arc;

use alloy::providers::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy::pubsub::SubscriptionStream;
use alloy_primitives::B256;
use alloy_rpc_types::{Filter, Header, Log};
use argus_types::{ArgusError, Result};
use futures::Stream;
use tracing::info;

/// Shared Alloy WebSocket provider.
///
/// Uses `RootProvider` so we keep a concrete, `Clone` handle that supports
/// pubsub subscriptions.
#[derive(Clone)]
pub struct AlloyProviderHandle {
    inner: RootProvider<alloy::pubsub::PubSubFrontend>,
}

impl AlloyProviderHandle {
    /// Subscribe to new block headers (`eth_subscribe("newHeads")`).
    pub async fn subscribe_blocks(&self) -> Result<SubscriptionStream<Header>> {
        let sub = self
            .inner
            .subscribe_blocks()
            .await
            .map_err(ArgusError::provider)?;
        Ok(sub.into_stream())
    }

    /// Subscribe to logs matching `filter` (`eth_subscribe("logs", filter)`).
    pub async fn subscribe_logs(&self, filter: &Filter) -> Result<SubscriptionStream<Log>> {
        let sub = self
            .inner
            .subscribe_logs(filter)
            .await
            .map_err(ArgusError::provider)?;
        Ok(sub.into_stream())
    }

    pub async fn get_block_number(&self) -> Result<u64> {
        self.inner
            .get_block_number()
            .await
            .map_err(ArgusError::provider)
    }

    pub fn provider(&self) -> &RootProvider<alloy::pubsub::PubSubFrontend> {
        &self.inner
    }
}

/// Connect to a WebSocket endpoint and return a handle.
pub async fn connect(ws_url: &str) -> Result<AlloyProviderHandle> {
    info!(%ws_url, "connecting Alloy WS provider");

    let ws = WsConnect::new(ws_url);
    let provider = ProviderBuilder::new()
        .on_ws(ws)
        .await
        .map_err(ArgusError::provider)?;

    Ok(AlloyProviderHandle { inner: provider })
}
