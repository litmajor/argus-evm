use std::collections::{HashMap, HashSet};

use alloy::eips::BlockId;
use alloy::providers::{Provider, ProviderBuilder, ReqwestProvider};
use alloy::rpc::types::{Filter, TransactionRequest};
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use anyhow::{Context, Result};
use argus_state::health::{compute_health_factor, estimate_repay_and_seize};
use argus_types::{
    CandidateId, CandidateTrigger, HealthFactor, LiquidationCandidate, Market, MarketId,
    OraclePrice, Position, Protocol,
};
use chrono::Utc;
use futures::{stream, StreamExt, TryStreamExt};
use tracing::{info, warn};

const LOG_BLOCK_RANGE: u64 = 10; // Alchemy Free-tier eth_getLogs limit.
const RPC_CONCURRENCY: usize = 8;
const WAD: u64 = 1_000_000_000_000_000_000;

sol! {
    event Borrow(
        bytes32 indexed id,
        address caller,
        address indexed onBehalf,
        address indexed receiver,
        uint256 assets,
        uint256 shares
    );

    interface IMorpho {
        function position(bytes32 id, address user) external view returns (
            uint128 supplyShares,
            uint128 borrowShares,
            uint128 collateral
        );

        function market(bytes32 id) external view returns (
            uint128 totalSupplyAssets,
            uint128 totalSupplyShares,
            uint128 totalBorrowAssets,
            uint128 totalBorrowShares,
            uint128 lastUpdate,
            uint128 fee
        );

        function idToMarketParams(bytes32 id) external view returns (
            address loanToken,
            address collateralToken,
            address oracle,
            address irm,
            uint256 lltv
        );
    }

    interface IMorphoOracle {
        function price() external view returns (uint256 value);
    }
}

#[derive(Clone)]
struct MarketSnapshot {
    market: Market,
    collateral_price: OraclePrice,
    loan_price: OraclePrice,
}

/// Scan historical Morpho borrow logs, then evaluate the borrowers against
/// their current on-chain position, market totals, and oracle price.
pub async fn discover_candidates(
    rpc_url: &str,
    morpho_address: Address,
    from_block: u64,
) -> Result<Vec<LiquidationCandidate>> {
    let url = rpc_url
        .parse()
        .with_context(|| format!("invalid HTTP RPC URL: {rpc_url}"))?;
    let provider = ProviderBuilder::new().on_http(url);
    let latest_block = provider
        .get_block_number()
        .await
        .context("failed to get latest block for Morpho discovery")?;
    anyhow::ensure!(
        from_block <= latest_block,
        "Morpho scan start block {from_block} is after latest block {latest_block}"
    );

    let mut borrowers = HashSet::<(B256, Address)>::new();
    let mut range_start = from_block;
    let mut scanned_logs = 0usize;

    loop {
        let range_end = range_start
            .saturating_add(LOG_BLOCK_RANGE - 1)
            .min(latest_block);
        let filter = Filter::new()
            .address(morpho_address)
            .event_signature(Borrow::SIGNATURE_HASH)
            .from_block(range_start)
            .to_block(range_end);
        let logs = provider.get_logs(&filter).await.with_context(|| {
            format!("failed to scan Morpho Borrow logs from {range_start} to {range_end}")
        })?;
        scanned_logs += logs.len();
        for log in logs {
            if let Ok(borrow) = Borrow::decode_log_data(log.data(), true) {
                borrowers.insert((borrow.id, borrow.onBehalf));
            } else {
                warn!(
                    block = log.block_number.unwrap_or_default(),
                    tx_hash = ?log.transaction_hash,
                    "skipping malformed Morpho Borrow log"
                );
            }
        }

        if range_end == latest_block {
            break;
        }
        range_start = range_end + 1;
    }

    info!(
        from_block,
        latest_block,
        scanned_logs,
        borrowers = borrowers.len(),
        "historical Morpho borrower scan complete"
    );

    let block = BlockId::Number(latest_block.into());
    let market_ids = borrowers
        .iter()
        .map(|(market_id, _)| *market_id)
        .collect::<HashSet<_>>();
    let markets = stream::iter(market_ids)
        .map(|market_id| {
            let provider = provider.clone();
            async move {
                let snapshot =
                    load_market_snapshot(&provider, morpho_address, market_id, block, latest_block)
                        .await?;
                Ok::<_, anyhow::Error>((market_id, snapshot))
            }
        })
        .buffer_unordered(RPC_CONCURRENCY)
        .try_collect::<HashMap<_, _>>()
        .await?;

    let candidates = stream::iter(borrowers)
        .map(|(market_id, borrower)| {
            let provider = provider.clone();
            let snapshot = markets.get(&market_id).cloned();
            async move {
                let snapshot = snapshot.ok_or_else(|| {
                    anyhow::anyhow!("missing snapshot for Morpho market {market_id:?}")
                })?;
                candidate_at_tip(
                    &provider,
                    morpho_address,
                    market_id,
                    borrower,
                    latest_block,
                    &snapshot,
                )
                .await
            }
        })
        .buffer_unordered(RPC_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;

    let candidates = candidates.into_iter().flatten().collect::<Vec<_>>();
    info!(
        count = candidates.len(),
        "liquidatable Morpho candidates discovered"
    );
    Ok(candidates)
}

async fn load_market_snapshot(
    provider: &ReqwestProvider,
    morpho_address: Address,
    market_id: B256,
    block: BlockId,
    block_number: u64,
) -> Result<MarketSnapshot> {
    let market_data = call_view(
        provider,
        morpho_address,
        IMorpho::marketCall { id: market_id },
        block,
    )
    .await
    .with_context(|| format!("failed to read Morpho market {market_id:?}"))?;
    let params = call_view(
        provider,
        morpho_address,
        IMorpho::idToMarketParamsCall { id: market_id },
        block,
    )
    .await
    .with_context(|| format!("failed to read Morpho market params {market_id:?}"))?;
    let oracle_price = call_view(provider, params.oracle, IMorphoOracle::priceCall {}, block)
        .await
        .with_context(|| format!("failed to read Morpho oracle at {:?}", params.oracle))?
        .value;
    let collateral_price = oracle_price / U256::from(WAD);
    anyhow::ensure!(
        !collateral_price.is_zero(),
        "Morpho oracle at {:?} returned a zero price",
        params.oracle
    );

    Ok(MarketSnapshot {
        market: Market {
            id: MarketId(market_id),
            protocol: Protocol::MorphoBlue,
            loan_token: params.loanToken,
            collateral_token: params.collateralToken,
            oracle: params.oracle,
            irm: params.irm,
            lltv: params.lltv,
            total_supply_assets: U256::from(market_data.totalSupplyAssets),
            total_supply_shares: U256::from(market_data.totalSupplyShares),
            total_borrow_assets: U256::from(market_data.totalBorrowAssets),
            total_borrow_shares: U256::from(market_data.totalBorrowShares),
            last_update: market_data.lastUpdate as u64,
            fee: U256::from(market_data.fee),
            updated_at: Utc::now(),
        },
        collateral_price: OraclePrice {
            asset: params.collateralToken,
            price: collateral_price,
            updated_at: block_number,
            source: Protocol::MorphoBlue,
        },
        loan_price: OraclePrice {
            asset: params.loanToken,
            price: U256::from(WAD),
            updated_at: block_number,
            source: Protocol::MorphoBlue,
        },
    })
}

async fn candidate_at_tip(
    provider: &ReqwestProvider,
    morpho_address: Address,
    market_id: B256,
    borrower: Address,
    block_number: u64,
    snapshot: &MarketSnapshot,
) -> Result<Option<LiquidationCandidate>> {
    let block = BlockId::Number(block_number.into());
    let position = call_view(
        provider,
        morpho_address,
        IMorpho::positionCall {
            id: market_id,
            user: borrower,
        },
        block,
    )
    .await
    .with_context(|| format!("failed to read Morpho position for {borrower:?}"))?;
    let borrow_shares = U256::from(position.borrowShares);
    if borrow_shares.is_zero() {
        return Ok(None);
    }
    if position.collateral == 0 {
        return Ok(None);
    }

    let market = &snapshot.market;
    let position = Position {
        market_id: MarketId(market_id),
        protocol: Protocol::MorphoBlue,
        user: borrower,
        supply_shares: U256::from(position.supplyShares),
        borrow_shares,
        collateral: U256::from(position.collateral),
        health_factor: HealthFactor::ONE,
        updated_at: Utc::now(),
        last_block: block_number,
    };
    let health_factor = compute_health_factor(
        &position,
        market,
        &snapshot.collateral_price,
        &snapshot.loan_price,
    );
    if !health_factor.is_liquidatable() {
        return Ok(None);
    }

    let (debt_to_cover, seize_assets) = estimate_repay_and_seize(
        &position,
        market,
        &snapshot.collateral_price,
        &snapshot.loan_price,
    );
    if debt_to_cover.is_zero() || seize_assets.is_zero() {
        return Ok(None);
    }

    Ok(Some(LiquidationCandidate {
        id: CandidateId::new(),
        protocol: Protocol::MorphoBlue,
        market_id: MarketId(market_id),
        borrower,
        health_factor,
        debt_to_cover,
        seize_assets,
        estimated_profit: seize_assets / U256::from(10u64),
        loan_token: market.loan_token,
        collateral_token: market.collateral_token,
        oracle: market.oracle,
        irm: market.irm,
        lltv: market.lltv,
        morpho: morpho_address,
        detected_at_block: block_number,
        detected_at: Utc::now(),
        trigger: CandidateTrigger::PeriodicScan,
    }))
}

async fn call_view<C: SolCall>(
    provider: &ReqwestProvider,
    address: Address,
    call: C,
    block: BlockId,
) -> Result<C::Return> {
    let request = TransactionRequest::default()
        .to(address)
        .input(call.abi_encode().into());
    let output = provider
        .call(&request)
        .block(block)
        .await
        .with_context(|| format!("eth_call to {address:?} failed"))?;
    C::abi_decode_returns(&output, true).context("failed to decode eth_call result")
}
