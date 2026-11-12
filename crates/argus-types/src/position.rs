//! User position inside a single market (SSA State model).

use alloy_primitives::{Address, U256};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{HealthFactor, MarketId, Protocol};

/// Per-user position. `user` is the account that holds the debt/collateral.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub market_id: MarketId,
    pub protocol: Protocol,
    /// Account that owns the position (borrower / violator).
    pub user: Address,
    pub supply_shares: U256,
    pub borrow_shares: U256,
    pub collateral: U256,
    /// Cached health factor (1e18 = 1.0). Recomputed on oracle/position updates.
    pub health_factor: HealthFactor,
    pub updated_at: DateTime<Utc>,
    pub last_block: u64,
}

impl Position {
    pub fn is_liquidatable(&self) -> bool {
        self.health_factor.is_liquidatable() && !self.borrow_shares.is_zero()
    }

    pub fn key(&self) -> PositionKey {
        PositionKey {
            market_id: self.market_id,
            user: self.user,
        }
    }
}

/// Composite key for O(1) position lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PositionKey {
    pub market_id: MarketId,
    pub user: Address,
}
