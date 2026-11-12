//! Ingestor configuration.

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestorConfig {
    /// WebSocket RPC endpoint (wss://...).
    pub ws_url: String,
    /// Morpho Blue core contract addresses to watch.
    pub morpho_blue_addresses: Vec<Address>,
    /// Euler v2 EVC / vault addresses to watch.
    pub euler_evc_addresses: Vec<Address>,
    /// Optional chain id for validation.
    pub chain_id: u64,
}

impl Default for IngestorConfig {
    fn default() -> Self {
        Self {
            // Placeholder — override via env / CLI
            ws_url: "wss://eth.llamarpc.com".to_string(),
            morpho_blue_addresses: vec![],
            euler_evc_addresses: vec![],
            chain_id: 1,
        }
    }
}
