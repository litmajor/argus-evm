//! Domain-level error types (thiserror).

use alloy_primitives::{Address, B256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArgusError {
    #[error("provider error: {0}")]
    Provider(String),

    #[error("subscription closed")]
    SubscriptionClosed,

    #[error("channel closed")]
    ChannelClosed,

    #[error("state: market {0} not found")]
    MarketNotFound(String),

    #[error("state: position {borrower:?} in market {market} not found")]
    PositionNotFound {
        borrower: Address,
        market: String,
    },

    #[error("simulation failed: {0}")]
    Simulation(String),

    #[error("execution rejected: {0}")]
    ExecutionRejected(String),

    #[error("unprofitable: expected_profit={expected} min_profit={min}")]
    Unprofitable { expected: String, min: String },

    #[error("revm error: {0}")]
    Revm(String),

    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("config: {0}")]
    Config(String),

    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, ArgusError>;

impl ArgusError {
    pub fn provider(e: impl ToString) -> Self {
        Self::Provider(e.to_string())
    }

    pub fn simulation(e: impl ToString) -> Self {
        Self::Simulation(e.to_string())
    }

    pub fn revm(e: impl ToString) -> Self {
        Self::Revm(e.to_string())
    }
}
