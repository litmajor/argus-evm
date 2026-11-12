//! Market state model (SSA State Layer).

use alloy_primitives::{Address, U256};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{HealthFactor, MarketId, OraclePrice, Protocol};

/// Full market snapshot kept in the hot in-memory state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub id: MarketId,
    pub protocol: Protocol,
    pub loan_token: Address,
    pub collateral_token: Address,
    pub oracle: Address,
    pub irm: Address,
    /// Loan-to-value threshold (Morpho LLTV, 1e18 scaled).
    pub lltv: U256,
    pub total_supply_assets: U256,
    pub total_supply_shares: U256,
    pub total_borrow_assets: U256,
    pub total_borrow_shares: U256,
    pub last_update: u64,
    pub fee: U256,
    pub updated_at: DateTime<Utc>,
}

impl Market {
    /// Rough utilization = total_borrow / total_supply (1e18).
    pub fn utilization(&self) -> U256 {
        if self.total_supply_assets.is_zero() {
            return U256::ZERO;
        }
        // borrow * 1e18 / supply
        self.total_borrow_assets
            .saturating_mul(U256::from(10u64.pow(18)))
            .checked_div(self.total_supply_assets)
            .unwrap_or(U256::ZERO)
    }
}

/// Live oracle view for a market pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketOracleView {
    pub market_id: MarketId,
    pub loan_price: OraclePrice,
    pub collateral_price: OraclePrice,
}
