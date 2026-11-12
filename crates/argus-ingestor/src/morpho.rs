//! Morpho Blue event decoding via alloy-sol-types.
//!
//! Event signatures (topic0) for Morpho Blue core:
//! - CreateMarket
//! - Borrow
//! - Repay
//! - Liquidate
//! - AccrueInterest (optional)

use alloy_primitives::{Address, B256, U256};
use alloy_rpc_types::Log;
use alloy_sol_types::{sol, SolEvent};
use argus_types::{
    BorrowEvent, DomainEvent, EventId, LiquidateEvent, MarketCreatedEvent, MarketId,
    Protocol, RepayEvent,
};
use chrono::Utc;
use tracing::trace;

// Alloy sol! macro — generates typed event structs + decode helpers.
sol! {
    /// Morpho Blue CreateMarket
    event CreateMarket(
        bytes32 indexed id,
        address indexed loanToken,
        address indexed collateralToken,
        address oracle,
        address irm,
        uint256 lltv
    );

    event Borrow(
        bytes32 indexed id,
        address caller,
        address indexed onBehalf,
        address indexed receiver,
        uint256 assets,
        uint256 shares
    );

    event Repay(
        bytes32 indexed id,
        address caller,
        address indexed onBehalf,
        uint256 assets,
        uint256 shares
    );

    event Liquidate(
        bytes32 indexed id,
        address indexed caller,
        address indexed borrower,
        uint256 repaidAssets,
        uint256 repaidShares,
        uint256 seizedAssets,
        uint256 badDebtAssets,
        uint256 badDebtShares
    );
}

pub fn decode_log(log: &Log) -> Option<DomainEvent> {
    let topics = log.topics();
    if topics.is_empty() {
        return None;
    }
    let topic0 = topics[0];

    if topic0 == CreateMarket::SIGNATURE_HASH {
        return decode_create_market(log);
    }
    if topic0 == Borrow::SIGNATURE_HASH {
        return decode_borrow(log);
    }
    if topic0 == Repay::SIGNATURE_HASH {
        return decode_repay(log);
    }
    if topic0 == Liquidate::SIGNATURE_HASH {
        return decode_liquidate(log);
    }

    trace!(?topic0, "unhandled Morpho topic");
    None
}

fn decode_create_market(log: &Log) -> Option<DomainEvent> {
    let decoded = CreateMarket::decode_log_data(log.data(), true).ok()?;
    Some(DomainEvent::MarketCreated(MarketCreatedEvent {
        id: EventId::new(),
        protocol: Protocol::MorphoBlue,
        market_id: MarketId(decoded.id),
        loan_token: decoded.loanToken,
        collateral_token: decoded.collateralToken,
        oracle: decoded.oracle,
        irm: decoded.irm,
        lltv: decoded.lltv,
        block_number: log.block_number.unwrap_or(0),
        tx_hash: log.transaction_hash.unwrap_or(B256::ZERO),
        log_index: log.log_index.unwrap_or(0),
        received_at: Utc::now(),
    }))
}

fn decode_borrow(log: &Log) -> Option<DomainEvent> {
    let decoded = Borrow::decode_log_data(log.data(), true).ok()?;
    Some(DomainEvent::Borrow(BorrowEvent {
        id: EventId::new(),
        protocol: Protocol::MorphoBlue,
        market_id: MarketId(decoded.id),
        borrower: decoded.onBehalf,
        assets: decoded.assets,
        shares: decoded.shares,
        on_behalf: decoded.onBehalf,
        receiver: decoded.receiver,
        block_number: log.block_number.unwrap_or(0),
        tx_hash: log.transaction_hash.unwrap_or(B256::ZERO),
        log_index: log.log_index.unwrap_or(0),
        received_at: Utc::now(),
    }))
}

fn decode_repay(log: &Log) -> Option<DomainEvent> {
    let decoded = Repay::decode_log_data(log.data(), true).ok()?;
    Some(DomainEvent::Repay(RepayEvent {
        id: EventId::new(),
        protocol: Protocol::MorphoBlue,
        market_id: MarketId(decoded.id),
        borrower: decoded.onBehalf,
        assets: decoded.assets,
        shares: decoded.shares,
        block_number: log.block_number.unwrap_or(0),
        tx_hash: log.transaction_hash.unwrap_or(B256::ZERO),
        log_index: log.log_index.unwrap_or(0),
        received_at: Utc::now(),
    }))
}

fn decode_liquidate(log: &Log) -> Option<DomainEvent> {
    let decoded = Liquidate::decode_log_data(log.data(), true).ok()?;
    Some(DomainEvent::Liquidate(LiquidateEvent {
        id: EventId::new(),
        protocol: Protocol::MorphoBlue,
        market_id: MarketId(decoded.id),
        liquidator: decoded.caller,
        borrower: decoded.borrower,
        repaid_assets: decoded.repaidAssets,
        seized_assets: decoded.seizedAssets,
        block_number: log.block_number.unwrap_or(0),
        tx_hash: log.transaction_hash.unwrap_or(B256::ZERO),
        log_index: log.log_index.unwrap_or(0),
        received_at: Utc::now(),
    }))
}
