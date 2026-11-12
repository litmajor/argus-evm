//! Euler v2 EVC event decoding skeleton.
//!
//! Euler v2 uses the Ethereum Vault Connector (EVC). Core events of interest:
//! - Borrow / Repay / Liquidate on vaults
//! - Oracle / price feed updates routed through EVC controllers
//!
//! Extend the sol! definitions with the exact ABIs from Euler's deployment.

use alloy_primitives::B256;
use alloy_rpc_types::Log;
use alloy_sol_types::{sol, SolEvent};
use argus_types::{
    DomainEvent, EventId, LiquidateEvent, MarketId, Protocol, BorrowEvent, RepayEvent,
};
use chrono::Utc;
use tracing::trace;

sol! {
    // Placeholder signatures — replace with exact Euler v2 EVC / vault events.
    event Borrow(
        address indexed account,
        address indexed vault,
        uint256 assets
    );

    event Repay(
        address indexed account,
        address indexed vault,
        uint256 assets
    );

    event Liquidate(
        address indexed liquidator,
        address indexed violator,
        address indexed collateral,
        address liability,
        uint256 repayAssets,
        uint256 seizeAssets
    );
}

pub fn decode_log(log: &Log) -> Option<DomainEvent> {
    let topics = log.topics();
    if topics.is_empty() {
        return None;
    }
    let topic0 = topics[0];

    if topic0 == Borrow::SIGNATURE_HASH {
        return decode_borrow(log);
    }
    if topic0 == Repay::SIGNATURE_HASH {
        return decode_repay(log);
    }
    if topic0 == Liquidate::SIGNATURE_HASH {
        return decode_liquidate(log);
    }

    trace!(?topic0, "unhandled Euler topic");
    None
}

fn decode_borrow(log: &Log) -> Option<DomainEvent> {
    let decoded = Borrow::decode_log_data(log.data(), true).ok()?;
    // Map vault → MarketId via address padding
    let market_id = MarketId::from_address(decoded.vault);
    Some(DomainEvent::Borrow(BorrowEvent {
        id: EventId::new(),
        protocol: Protocol::EulerV2,
        market_id,
        borrower: decoded.account,
        assets: decoded.assets,
        shares: alloy_primitives::U256::ZERO, // Euler may use assets only
        on_behalf: decoded.account,
        receiver: decoded.account,
        block_number: log.block_number.unwrap_or(0),
        tx_hash: log.transaction_hash.unwrap_or(B256::ZERO),
        log_index: log.log_index.unwrap_or(0),
        received_at: Utc::now(),
    }))
}

fn decode_repay(log: &Log) -> Option<DomainEvent> {
    let decoded = Repay::decode_log_data(log.data(), true).ok()?;
    let market_id = MarketId::from_address(decoded.vault);
    Some(DomainEvent::Repay(RepayEvent {
        id: EventId::new(),
        protocol: Protocol::EulerV2,
        market_id,
        borrower: decoded.account,
        assets: decoded.assets,
        shares: alloy_primitives::U256::ZERO,
        block_number: log.block_number.unwrap_or(0),
        tx_hash: log.transaction_hash.unwrap_or(B256::ZERO),
        log_index: log.log_index.unwrap_or(0),
        received_at: Utc::now(),
    }))
}

fn decode_liquidate(log: &Log) -> Option<DomainEvent> {
    let decoded = Liquidate::decode_log_data(log.data(), true).ok()?;
    let market_id = MarketId::from_address(decoded.liability);
    Some(DomainEvent::Liquidate(LiquidateEvent {
        id: EventId::new(),
        protocol: Protocol::EulerV2,
        market_id,
        liquidator: decoded.liquidator,
        borrower: decoded.violator,
        repaid_assets: decoded.repayAssets,
        seized_assets: decoded.seizeAssets,
        block_number: log.block_number.unwrap_or(0),
        tx_hash: log.transaction_hash.unwrap_or(B256::ZERO),
        log_index: log.log_index.unwrap_or(0),
        received_at: Utc::now(),
    }))
}
