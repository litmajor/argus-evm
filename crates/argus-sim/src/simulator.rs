//! Core revm simulation of the atomic liquidation path.
//!
//! 1. Build calldata for `VaultLiquidator.executeLiquidation`
//! 2. Use a provider-backed database to load fork state at a pinned tip
//! 3. `Evm::transact` against a cached fork database

use std::time::Instant;

use alloy::eips::BlockId;
use alloy_primitives::{aliases::U24, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use argus_types::{ArgusError, LiquidationCandidate, Result, SimulationResult};
use chrono::Utc;
use revm::db::CacheDB;
use revm::primitives::{ExecutionResult, Output, SpecId, TxKind, U256 as RevmU256};
use revm::{DatabaseRef, Evm};
use tracing::{debug, info, warn};

use crate::alloy_db::ProviderDb;
use crate::config::SimConfig;
use crate::fork::ForkState;
use crate::morpho_calldata::{IVaultLiquidator, MorphoLiquidationData};

sol! {
    event LiquidationExecuted(
        address indexed borrower,
        address indexed loanToken,
        address indexed collateralToken,
        uint256 debtRepaid,
        uint256 profit
    );
}

pub struct Simulator {
    config: SimConfig,
}

impl Simulator {
    pub fn new(config: SimConfig) -> Self {
        Self { config }
    }

    /// Simulate one candidate against live state pinned to the current tip.
    pub async fn simulate(&self, candidate: &LiquidationCandidate) -> Result<SimulationResult> {
        let start = Instant::now();

        // Exact Morpho Blue liquidate params (seize-specified, repaidShares = 0).
        let morpho = MorphoLiquidationData::from_candidate_seize(candidate);
        let call = IVaultLiquidator::executeLiquidationCall {
            mp: morpho.market_params,
            borrower: candidate.borrower,
            seizedAssets: morpho.seized_assets,
            repaidShares: morpho.repaid_shares,
            minNetProfit: self.config.min_profit,
            poolFee: U24::ZERO,
        };
        let calldata = call.abi_encode();

        // ── Live fork at tip ───────────────────────────────────────────
        let fork = ForkState::fork_tip(&self.config.http_rpc_url).await?;
        let provider = fork.provider().ok_or_else(|| {
            ArgusError::provider(
                "could not connect to RPC; refusing to simulate without live fork state",
            )
        })?;
        let overlay = fork.overlay();

        let provider_db =
            ProviderDb::new(provider.clone(), BlockId::Number(fork.block_number.into()))?;
        let mut cache_db = CacheDB::new(provider_db);
        let (success, gas_used, net_profit_wei, failure_reason) =
            self.run_revm(&mut cache_db, fork.block_number, &calldata, candidate)?;

        let elapsed = start.elapsed();
        let gas_cost = self.config.gas_price.saturating_mul(U256::from(gas_used));
        let result = SimulationResult {
            candidate_id: candidate.id,
            candidate: candidate.clone(),
            success,
            net_profit_wei,
            min_profit: self.config.min_profit,
            gas_used,
            gas_cost,
            flash_loan_fee: U256::ZERO,
            calldata,
            target: self.config.liquidator,
            fork_block: overlay.block_number,
            simulated_at: Utc::now(),
            failure_reason,
        };

        info!(
            candidate = %candidate.id,
            success = result.success,
            profit = %result.net_profit_wei,
            gas = gas_used,
            fork_block = overlay.block_number,
            live = overlay.live,
            latency_us = elapsed.as_micros(),
            "simulation complete"
        );

        if elapsed.as_millis() > 2 {
            debug!(
                latency_ms = elapsed.as_millis(),
                "simulation exceeded 2ms target"
            );
        }

        Ok(result)
    }

    /// `Evm::transact` against a lazy provider-backed fork at `block_number`.
    fn run_revm<DB: DatabaseRef>(
        &self,
        cache_db: &mut CacheDB<DB>,
        block_number: u64,
        calldata: &[u8],
        candidate: &LiquidationCandidate,
    ) -> Result<(bool, u64, U256, Option<String>)>
    where
        DB::Error: std::fmt::Debug,
    {
        if candidate.debt_to_cover.is_zero() {
            return Ok((false, 0, U256::ZERO, Some("zero debt_to_cover".into())));
        }

        debug!(
            block_number,
            "running Evm::transact against lazy provider-backed fork"
        );

        let to = self.config.liquidator;

        let transact_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut evm = Evm::builder()
                .with_db(cache_db)
                .with_spec_id(SpecId::CANCUN)
                .modify_cfg_env(|cfg| {
                    cfg.chain_id = self.config.chain_id;
                })
                .modify_block_env(|block| {
                    block.number = RevmU256::from(block_number);
                    block.basefee = RevmU256::ZERO;
                })
                .modify_tx_env(|tx| {
                    tx.caller = self.config.caller;
                    tx.transact_to = TxKind::Call(to);
                    tx.data = calldata.to_vec().into();
                    tx.value = RevmU256::ZERO;
                    tx.gas_limit = 1_000_000;
                    tx.gas_price = RevmU256::from(1u64);
                })
                .build();

            evm.transact()
        }));

        match transact_result {
            Ok(Ok(result_and_state)) => match result_and_state.result {
                ExecutionResult::Success {
                    output,
                    logs,
                    gas_used,
                    ..
                } => {
                    let net_profit = logs
                        .iter()
                        .filter(|log| {
                            log.address == self.config.liquidator
                                && log.data.topics().first()
                                    == Some(&LiquidationExecuted::SIGNATURE_HASH)
                        })
                        .find_map(|log| {
                            LiquidationExecuted::decode_log_data(&log.data, true)
                                .ok()
                                .map(|event| event.profit)
                        });
                    let Some(net_profit) = net_profit else {
                        let output_bytes = match output {
                            Output::Call(data) => data.len(),
                            Output::Create(data, _) => data.len(),
                        };
                        return Ok((
                            false,
                            gas_used,
                            U256::ZERO,
                            Some(format!(
                                "liquidation succeeded without LiquidationExecuted event (return data: {output_bytes} bytes)"
                            )),
                        ));
                    };

                    if net_profit >= self.config.min_profit {
                        Ok((true, gas_used, net_profit, None))
                    } else {
                        Ok((
                            false,
                            gas_used,
                            net_profit,
                            Some("below min_profit after sim".into()),
                        ))
                    }
                }
                ExecutionResult::Revert {
                    output, gas_used, ..
                } => {
                    let reason = decode_revert(&output);
                    Ok((false, gas_used, U256::ZERO, Some(reason)))
                }
                ExecutionResult::Halt {
                    reason, gas_used, ..
                } => Ok((
                    false,
                    gas_used,
                    U256::ZERO,
                    Some(format!("halt: {reason:?}")),
                )),
            },
            Ok(Err(e)) => {
                warn!(error = ?e, "Evm::transact error");
                Ok((false, 0, U256::ZERO, Some(format!("evm error: {e:?}"))))
            }
            Err(panic) => {
                let detail = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("unknown panic");
                let message = format!("Evm::transact panicked: {detail}");
                warn!(%message, "simulation failed");
                Err(ArgusError::revm(message))
            }
        }
    }
}

fn decode_revert(output: &revm::primitives::Bytes) -> String {
    if output.len() >= 68 && output[0..4] == [0x08, 0xc3, 0x79, 0xa0] {
        let len = U256::from_be_slice(&output[36..68])
            .try_into()
            .unwrap_or(0usize)
            .min(output.len().saturating_sub(68));
        let s = String::from_utf8_lossy(&output[68..68 + len]);
        format!("revert: {s}")
    } else if output.is_empty() {
        "revert (empty)".into()
    } else {
        format!("revert: 0x{}", hex::encode(&output[..output.len().min(32)]))
    }
}
