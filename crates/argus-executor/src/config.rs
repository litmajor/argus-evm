use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

/// Which private submission path to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubmitMode {
    /// Flashbots-style `eth_sendBundle` (array of signed txs + block target).
    #[default]
    FlashbotsBundle,
    /// Single private tx (`eth_sendPrivateTransaction` / Protect-style).
    PrivateTx,
    /// Generic `eth_sendRawTransaction` to a private RPC (no public mempool).
    PrivateRpc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorConfig {
    /// Private RPC / builder endpoint (Flashbots relay, Titan, Beaver, etc.).
    pub private_rpc_url: String,
    /// Optional public RPC for receipt polling / nonce reads.
    pub public_rpc_url: Option<String>,
    /// Signer private key (hex, with or without 0x). Load from env / HSM in prod.
    pub private_key_hex: String,
    /// Chain id.
    pub chain_id: u64,
    /// Path to append-only JSONL telemetry file.
    pub telemetry_path: String,
    /// Max priority fee (wei).
    pub max_priority_fee: U256,
    /// Max fee (wei).
    pub max_fee: U256,
    /// Gas limit ceiling.
    pub gas_limit: u64,
    /// Optional builder payment (coinbase transfer in wei).
    pub builder_payment: Option<U256>,
    /// Submission mode.
    pub submit_mode: SubmitMode,
    /// Flashbots-style auth: optional signing key for X-Flashbots-Signature
    /// (often same as private_key_hex; leave empty to skip header).
    pub flashbots_auth_key_hex: Option<String>,
    /// Target blocks ahead of tip for `eth_sendBundle` (default 1).
    pub bundle_blocks_ahead: u64,
    /// Seconds to wait for a receipt after submission.
    pub receipt_timeout_secs: u64,
    /// Milliseconds between receipt polls.
    pub receipt_poll_ms: u64,
    /// If true, block until receipt (or timeout) before returning outcome.
    pub wait_for_receipt: bool,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            private_rpc_url: "https://relay.flashbots.net".to_string(),
            public_rpc_url: Some("https://eth.llamarpc.com".to_string()),
            private_key_hex: String::new(),
            chain_id: 1,
            telemetry_path: "telemetry/argus-evm.jsonl".to_string(),
            max_priority_fee: U256::from(1_000_000_000u64), // 1 gwei
            max_fee: U256::from(50_000_000_000u64),         // 50 gwei
            gas_limit: 800_000,
            builder_payment: None,
            submit_mode: SubmitMode::FlashbotsBundle,
            flashbots_auth_key_hex: None,
            bundle_blocks_ahead: 1,
            receipt_timeout_secs: 60,
            receipt_poll_ms: 1_200,
            wait_for_receipt: true,
        }
    }
}
