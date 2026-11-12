//! Domain events emitted by the Ingestor (SSA Actions layer).
//!
//! These are the normalized events that flow over the broadcast channel.
//! Surfaces (state, sim, executor) subscribe and react.

use alloy_primitives::{Address, B256, LogData, U256};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{BlockHeader, HealthFactor, MarketId, OraclePrice, Protocol};

/// Unique event id for telemetry correlation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

/// Top-level domain event envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DomainEvent {
    NewHead(NewHeadEvent),
    MarketCreated(MarketCreatedEvent),
    Borrow(BorrowEvent),
    Repay(RepayEvent),
    Liquidate(LiquidateEvent),
    OracleUpdate(OracleUpdateEvent),
    /// Catch-all for protocol-specific logs we still want to surface.
    RawLog(RawLogEvent),
}

impl DomainEvent {
    pub fn id(&self) -> EventId {
        match self {
            Self::NewHead(e) => e.id,
            Self::MarketCreated(e) => e.id,
            Self::Borrow(e) => e.id,
            Self::Repay(e) => e.id,
            Self::Liquidate(e) => e.id,
            Self::OracleUpdate(e) => e.id,
            Self::RawLog(e) => e.id,
        }
    }

    pub fn block_number(&self) -> Option<u64> {
        match self {
            Self::NewHead(e) => Some(e.header.number),
            Self::MarketCreated(e) => Some(e.block_number),
            Self::Borrow(e) => Some(e.block_number),
            Self::Repay(e) => Some(e.block_number),
            Self::Liquidate(e) => Some(e.block_number),
            Self::OracleUpdate(e) => Some(e.block_number),
            Self::RawLog(e) => Some(e.block_number),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewHeadEvent {
    pub id: EventId,
    pub header: BlockHeader,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketCreatedEvent {
    pub id: EventId,
    pub protocol: Protocol,
    pub market_id: MarketId,
    pub loan_token: Address,
    pub collateral_token: Address,
    pub oracle: Address,
    pub irm: Address,
    pub lltv: U256,
    pub block_number: u64,
    pub tx_hash: B256,
    pub log_index: u64,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BorrowEvent {
    pub id: EventId,
    pub protocol: Protocol,
    pub market_id: MarketId,
    pub borrower: Address,
    pub assets: U256,
    pub shares: U256,
    pub on_behalf: Address,
    pub receiver: Address,
    pub block_number: u64,
    pub tx_hash: B256,
    pub log_index: u64,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepayEvent {
    pub id: EventId,
    pub protocol: Protocol,
    pub market_id: MarketId,
    pub borrower: Address,
    pub assets: U256,
    pub shares: U256,
    pub block_number: u64,
    pub tx_hash: B256,
    pub log_index: u64,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidateEvent {
    pub id: EventId,
    pub protocol: Protocol,
    pub market_id: MarketId,
    pub liquidator: Address,
    pub borrower: Address,
    pub repaid_assets: U256,
    pub seized_assets: U256,
    pub block_number: u64,
    pub tx_hash: B256,
    pub log_index: u64,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleUpdateEvent {
    pub id: EventId,
    pub protocol: Protocol,
    pub oracle: Address,
    pub asset: Address,
    pub price: U256,
    pub block_number: u64,
    pub tx_hash: B256,
    pub log_index: u64,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawLogEvent {
    pub id: EventId,
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Vec<u8>,
    pub block_number: u64,
    pub tx_hash: B256,
    pub log_index: u64,
    pub received_at: DateTime<Utc>,
}
