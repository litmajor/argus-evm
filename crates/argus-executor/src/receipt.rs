//! Receipt polling after private-bundle / private-tx submission.
//!
//! Polls `eth_getTransactionReceipt` on the public RPC until inclusion,
//! timeout, or explicit failure. Updates `ExecutionOutcome` with block
//! number and final success bit.

use std::time::{Duration, Instant};

use alloy_primitives::B256;
use argus_types::{ArgusError, ExecutionOutcome, Result};
use chrono::Utc;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::config::ExecutorConfig;

#[derive(Debug, Clone)]
pub struct ReceiptPoller {
    http: reqwest::Client,
    public_rpc: String,
    /// Max time to wait for inclusion.
    timeout: Duration,
    /// Interval between polls.
    interval: Duration,
}

impl ReceiptPoller {
    pub fn new(config: &ExecutorConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            public_rpc: config
                .public_rpc_url
                .clone()
                .unwrap_or_else(|| config.private_rpc_url.clone()),
            timeout: Duration::from_secs(config.receipt_timeout_secs),
            interval: Duration::from_millis(config.receipt_poll_ms),
        }
    }

    /// Poll until receipt appears or timeout.
    ///
    /// Returns an updated outcome with `block_number`, `confirmed_at`, and
    /// final `success` based on receipt status.
    pub async fn wait_for_receipt(
        &self,
        mut outcome: ExecutionOutcome,
    ) -> Result<ExecutionOutcome> {
        let Some(tx_hash) = outcome.tx_hash else {
            return Ok(outcome);
        };

        let deadline = Instant::now() + self.timeout;

        loop {
            match self.get_receipt(tx_hash).await {
                Ok(Some(receipt)) => {
                    let status_ok = receipt
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(|s| s == "0x1" || s == "0x01")
                        .unwrap_or(false);

                    let block_number = receipt
                        .get("blockNumber")
                        .and_then(|v| v.as_str())
                        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());

                    let gas_used = receipt
                        .get("gasUsed")
                        .and_then(|v| v.as_str())
                        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());

                    outcome.success = status_ok;
                    outcome.block_number = block_number;
                    outcome.confirmed_at = Some(Utc::now());
                    if !status_ok {
                        outcome.error = Some(format!(
                            "tx reverted on-chain (gas_used={gas_used:?})"
                        ));
                    }

                    info!(
                        tx = %tx_hash,
                        block = ?block_number,
                        ok = status_ok,
                        "receipt confirmed"
                    );

                    return Ok(outcome);
                }
                Ok(None) => {
                    debug!(tx = %tx_hash, "receipt not yet available");
                }
                Err(e) => {
                    warn!(tx = %tx_hash, error = %e, "receipt poll error");
                }
            }

            if Instant::now() >= deadline {
                outcome.error = Some(format!(
                    "receipt timeout after {}s",
                    self.timeout.as_secs()
                ));
                // Keep success=true if submission itself succeeded — inclusion pending.
                return Ok(outcome);
            }

            tokio::time::sleep(self.interval).await;
        }
    }

    async fn get_receipt(&self, tx_hash: B256) -> Result<Option<Value>> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getTransactionReceipt",
            "params": [format!("{tx_hash:#x}")]
        });

        let resp: Value = self
            .http
            .post(&self.public_rpc)
            .json(&body)
            .send()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?;

        if let Some(err) = resp.get("error") {
            return Err(ArgusError::provider(err.to_string()));
        }

        match resp.get("result") {
            Some(Value::Null) | None => Ok(None),
            Some(r) => Ok(Some(r.clone())),
        }
    }
}
