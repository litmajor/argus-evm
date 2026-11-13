//! Signed private-bundle / private-tx submission.
//!
//! Supports three modes (see `SubmitMode`):
//! 1. **FlashbotsBundle** — `eth_sendBundle` with signed raw txs + target block
//! 2. **PrivateTx** — `eth_sendPrivateTransaction` (Protect-style)
//! 3. **PrivateRpc** — `eth_sendRawTransaction` to a non-public endpoint
//!
//! Signing uses `alloy-signer-local`. HTTP uses `reqwest` via Alloy's transport
//! or a thin JSON-RPC client so we stay independent of a specific builder SDK.

use std::time::Instant;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use argus_types::{ArgusError, ExecutionBundle, ExecutionOutcome, Result, SimulationResult};
use chrono::Utc;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::config::{ExecutorConfig, SubmitMode};

pub struct BundleSubmitter {
    config: ExecutorConfig,
    signer: Option<PrivateKeySigner>,
    http: reqwest::Client,
}

impl BundleSubmitter {
    pub fn new(config: ExecutorConfig) -> Self {
        let signer = if config.private_key_hex.is_empty() {
            None
        } else {
            let hex = config.private_key_hex.trim_start_matches("0x");
            match hex.parse::<PrivateKeySigner>() {
                Ok(s) => Some(s),
                Err(e) => {
                    warn!(error = %e, "failed to parse private key — submissions disabled");
                    None
                }
            }
        };

        Self {
            config,
            signer,
            http: reqwest::Client::new(),
        }
    }

    pub fn address(&self) -> Option<Address> {
        self.signer.as_ref().map(|s| s.address())
    }

    pub async fn submit(&self, sim: &SimulationResult) -> Result<ExecutionOutcome> {
        let start = Instant::now();

        let gas_limit = self
            .config
            .gas_limit
            .min(sim.gas_used.saturating_mul(12) / 10); // 20% headroom

        let bundle = ExecutionBundle {
            candidate_id: sim.candidate_id,
            target: sim.target,
            calldata: sim.calldata.clone(),
            value: U256::ZERO,
            max_priority_fee_per_gas: self.config.max_priority_fee,
            max_fee_per_gas: self.config.max_fee,
            gas_limit,
            chain_id: self.config.chain_id,
            builder_payment: self.config.builder_payment,
            simulation: sim.clone(),
        };

        let outcome = self.send_bundle(&bundle).await?;

        info!(
            candidate = %sim.candidate_id,
            success = outcome.success,
            tx = ?outcome.tx_hash,
            mode = ?self.config.submit_mode,
            latency_us = start.elapsed().as_micros(),
            "bundle submitted"
        );

        Ok(outcome)
    }

    async fn send_bundle(&self, bundle: &ExecutionBundle) -> Result<ExecutionOutcome> {
        let signer = match &self.signer {
            Some(s) => s,
            None => {
                return Ok(ExecutionOutcome {
                    candidate_id: bundle.candidate_id,
                    success: false,
                    tx_hash: None,
                    block_number: None,
                    error: Some("no private key configured".into()),
                    submitted_at: Utc::now(),
                    confirmed_at: None,
                });
            }
        };

        // 1. Build + sign the raw transaction
        let (raw_tx, tx_hash) = self.sign_tx(signer, bundle).await?;

        // 2. Dispatch according to mode
        let rpc_result = match self.config.submit_mode {
            SubmitMode::FlashbotsBundle => self.eth_send_bundle(&raw_tx, bundle).await,
            SubmitMode::PrivateTx => self.eth_send_private_transaction(&raw_tx).await,
            SubmitMode::PrivateRpc => self.eth_send_raw_transaction(&raw_tx).await,
        };

        match rpc_result {
            Ok(hash) => Ok(ExecutionOutcome {
                candidate_id: bundle.candidate_id,
                success: true,
                tx_hash: Some(hash.unwrap_or(tx_hash)),
                block_number: None, // filled by receipt poller (future)
                error: None,
                submitted_at: Utc::now(),
                confirmed_at: None,
            }),
            Err(e) => Ok(ExecutionOutcome {
                candidate_id: bundle.candidate_id,
                success: false,
                tx_hash: Some(tx_hash),
                block_number: None,
                error: Some(e.to_string()),
                submitted_at: Utc::now(),
                confirmed_at: None,
            }),
        }
    }

    /// Sign an EIP-1559 tx for `VaultLiquidator.executeLiquidation`.
    async fn sign_tx(
        &self,
        signer: &PrivateKeySigner,
        bundle: &ExecutionBundle,
    ) -> Result<(Bytes, B256)> {
        use alloy::consensus::{SignableTransaction, TxEip1559};
        use alloy::network::TxSignerSync;
        use alloy_primitives::{TxKind, U256};

        // Nonce: in production read from public RPC. Skeleton uses 0.
        let nonce = self.fetch_nonce(signer.address()).await.unwrap_or(0);

        let mut tx = TxEip1559 {
            chain_id: bundle.chain_id,
            nonce,
            gas_limit: bundle.gas_limit,
            max_fee_per_gas: bundle.max_fee_per_gas.try_into().unwrap_or(u128::MAX),
            max_priority_fee_per_gas: bundle
                .max_priority_fee_per_gas
                .try_into()
                .unwrap_or(u128::MAX),
            to: TxKind::Call(bundle.target),
            value: bundle.value,
            access_list: Default::default(),
            input: bundle.calldata.clone().into(),
        };

        let sig = signer
            .sign_transaction_sync(&mut tx)
            .map_err(|e| ArgusError::Internal(format!("sign error: {e}")))?;

        let signed = tx.into_signed(sig);
        let mut encoded = Vec::new();
        signed.rlp_encode(&mut encoded);
        // EIP-1559 typed tx prefix
        let mut raw = vec![0x02];
        raw.extend_from_slice(&encoded);

        let tx_hash = B256::from_slice(&alloy_primitives::keccak256(&raw)[..]);
        Ok((Bytes::from(raw), tx_hash))
    }

    async fn fetch_nonce(&self, address: Address) -> Result<u64> {
        let url = self
            .config
            .public_rpc_url
            .as_deref()
            .unwrap_or(&self.config.private_rpc_url);

        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getTransactionCount",
            "params": [format!("{address:#x}"), "pending"]
        });

        let resp: Value = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?;

        let hex = resp
            .get("result")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ArgusError::provider("nonce: missing result"))?;

        let n = u64::from_str_radix(hex.trim_start_matches("0x"), 16)
            .map_err(|e| ArgusError::provider(format!("nonce parse: {e}")))?;
        Ok(n)
    }

    async fn tip_block_number(&self) -> Result<u64> {
        let url = self
            .config
            .public_rpc_url
            .as_deref()
            .unwrap_or(&self.config.private_rpc_url);

        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_blockNumber",
            "params": []
        });

        let resp: Value = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?;

        let hex = resp
            .get("result")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ArgusError::provider("blockNumber: missing result"))?;

        u64::from_str_radix(hex.trim_start_matches("0x"), 16)
            .map_err(|e| ArgusError::provider(format!("blockNumber parse: {e}")))
    }

    /// Flashbots / builder `eth_sendBundle`.
    async fn eth_send_bundle(
        &self,
        raw_tx: &Bytes,
        _bundle: &ExecutionBundle,
    ) -> Result<Option<B256>> {
        let tip = self.tip_block_number().await.unwrap_or(0);
        let target_block = tip + self.config.bundle_blocks_ahead;

        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendBundle",
            "params": [{
                "txs": [format!("0x{}", hex::encode(raw_tx))],
                "blockNumber": format!("0x{target_block:x}"),
                "minTimestamp": 0,
                "maxTimestamp": 0,
            }]
        });

        let mut req = self.http.post(&self.config.private_rpc_url).json(&body);

        // Optional Flashbots auth header (signature over body hash).
        if let Some(auth_key) = &self.config.flashbots_auth_key_hex {
            if let Ok(auth_signer) = auth_key
                .trim_start_matches("0x")
                .parse::<PrivateKeySigner>()
            {
                let body_hash = alloy_primitives::keccak256(body.to_string().as_bytes());
                // Header format: {address}:{signature} — simplified; production
                // uses the exact Flashbots payload signing scheme.
                req = req.header(
                    "X-Flashbots-Signature",
                    format!("{:#x}:0x{}", auth_signer.address(), hex::encode(body_hash)),
                );
            }
        }

        let resp: Value = req
            .send()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?;

        if let Some(err) = resp.get("error") {
            return Err(ArgusError::ExecutionRejected(err.to_string()));
        }

        // Bundle hash if returned
        let hash = resp
            .pointer("/result/bundleHash")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<B256>().ok());

        Ok(hash)
    }

    /// Protect-style private transaction.
    async fn eth_send_private_transaction(&self, raw_tx: &Bytes) -> Result<Option<B256>> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendPrivateTransaction",
            "params": [{
                "tx": format!("0x{}", hex::encode(raw_tx)),
                "maxBlockNumber": null,
                "preferences": {
                    "fast": true
                }
            }]
        });

        let resp: Value = self
            .http
            .post(&self.config.private_rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?;

        if let Some(err) = resp.get("error") {
            return Err(ArgusError::ExecutionRejected(err.to_string()));
        }

        let hash = resp
            .get("result")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<B256>().ok());

        Ok(hash)
    }

    /// Generic private RPC raw send.
    async fn eth_send_raw_transaction(&self, raw_tx: &Bytes) -> Result<Option<B256>> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendRawTransaction",
            "params": [format!("0x{}", hex::encode(raw_tx))]
        });

        let resp: Value = self
            .http
            .post(&self.config.private_rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| ArgusError::provider(e.to_string()))?;

        if let Some(err) = resp.get("error") {
            return Err(ArgusError::ExecutionRejected(err.to_string()));
        }

        let hash = resp
            .get("result")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<B256>().ok());

        Ok(hash)
    }
}
