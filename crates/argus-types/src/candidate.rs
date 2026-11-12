//! Liquidation candidates produced by the State domain.

use alloy_primitives::{Address, U256};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{HealthFactor, MarketId, Protocol};

/// Unique candidate id for end-to-end telemetry correlation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateId(pub Uuid);

impl CandidateId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for CandidateId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for CandidateId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A position that has breached the health-factor threshold.
/// Emitted by `argus-state` into the candidate mpsc channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidationCandidate {
    pub id: CandidateId,
    pub protocol: Protocol,
    pub market_id: MarketId,
    pub borrower: Address,
    pub health_factor: HealthFactor,
    /// Debt the liquidator intends to cover (protocol close-factor applied).
    pub debt_to_cover: U256,
    /// Estimated collateral that would be seized.
    pub seize_assets: U256,
    /// Pre-sim rough profit estimate (loan-token units). Refined by argus-sim.
    pub estimated_profit: U256,
    /// Loan token (debt asset) — also the flash-loan asset.
    pub loan_token: Address,
    /// Collateral token.
    pub collateral_token: Address,
    /// Morpho market params (needed for liquidate MarketParams).
    pub oracle: Address,
    pub irm: Address,
    pub lltv: U256,
    /// Morpho Blue core contract (not the market id).
    pub morpho: Address,
    /// Block number at which the candidate was detected.
    pub detected_at_block: u64,
    pub detected_at: DateTime<Utc>,
    pub trigger: CandidateTrigger,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateTrigger {
    OracleUpdate { asset: Address },
    Borrow,
    MarketParamChange,
    PeriodicScan,
    Other(String),
}
