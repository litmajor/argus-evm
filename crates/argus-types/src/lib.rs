//! # argus-types — SSA State Layer
//!
//! Shared domain language for the entire argus-evm workspace.
//! Every other crate (ingestor, state, sim, executor) depends on these types.
//!
//! No I/O. No RPC. No side-effects.
//!
//! Design goals:
//! - alloy-primitives for all EVM values (Address, U256, B256)
//! - thiserror for domain errors
//! - Serde for JSONL telemetry and IPC
//! - Newtypes where they prevent unit mistakes (MarketId, HealthFactor)

pub mod error;
pub mod events;
pub mod market;
pub mod position;
pub mod candidate;
pub mod sim;
pub mod telemetry;

pub use error::*;
pub use events::*;
pub use market::*;
pub use position::*;
pub use candidate::*;
pub use sim::*;
pub use telemetry::*;

use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};
use std::fmt;

// ─────────────────────────────────────────────────────────────
// Core identifiers
// ─────────────────────────────────────────────────────────────

/// Opaque market identifier.
/// - Morpho Blue: the 32-byte market `id`
/// - Euler v2: vault address left-padded into B256
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MarketId(pub B256);

impl MarketId {
    pub fn from_address(addr: Address) -> Self {
        let mut bytes = [0u8; 32];
        bytes[12..].copy_from_slice(addr.as_slice());
        Self(B256::from(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(B256::from(bytes))
    }

    pub fn as_b256(&self) -> B256 {
        self.0
    }
}

impl fmt::Debug for MarketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MarketId(0x{}…)", hex::encode(&self.0[..4]))
    }
}

impl fmt::Display for MarketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

/// Protocol family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    MorphoBlue,
    EulerV2,
    Other,
}

// ─────────────────────────────────────────────────────────────
// Health factor (1e18 = 1.0)
// ─────────────────────────────────────────────────────────────

/// Fixed-point health factor. Values **below** `ONE` (1e18) are liquidatable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HealthFactor(pub U256);

impl HealthFactor {
    /// 1.0 in 1e18 fixed-point.
    pub const ONE: Self = Self(U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]));

    pub fn from_raw(raw: U256) -> Self {
        Self(raw)
    }

    pub fn is_liquidatable(&self) -> bool {
        self.0 < Self::ONE.0
    }

    /// Lossy f64 for logs / dashboards only.
    pub fn as_f64_approx(&self) -> f64 {
        let limbs = self.0.as_limbs();
        (limbs[0] as f64) / 1e18
    }
}

impl fmt::Display for HealthFactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.6}", self.as_f64_approx())
    }
}

// ─────────────────────────────────────────────────────────────
// Block header snapshot
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub timestamp: u64,
    pub base_fee_per_gas: Option<U256>,
}

// ─────────────────────────────────────────────────────────────
// Oracle price
// ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OraclePrice {
    pub asset: Address,
    pub price: U256,
    pub updated_at: u64,
    pub source: Protocol,
}
