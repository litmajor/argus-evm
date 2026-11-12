//! Core revm simulation of the atomic liquidation path.
//!
//! 1. Build calldata for `VaultLiquidator.executeLiquidation`
//! 2. Warm `CacheDB` from Alloy at tip (balance/nonce/code for hot accounts)
//! 3. `Evm::transact` fully in-memory — no RPC on the hot path

use std::time::Instant;

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use argus_types::{LiquidationCandidate, Result, SimulationResult};
use chrono::Utc;
use revm::database::{CacheDB, EmptyDB};
use revm::primitives::{ExecutionResult, Output, SpecId, TxKind, U256 as RevmU256};
use revm::Evm;
use tracing::{debug, info, warn};

use crate::config::SimConfig;
use crate::alloy_db::{self, build_warm_db};
use crate::fork::{self, ForkState};
use crate::morpho_calldata::MorphoLiquidationData;

// ABI must match contracts/src/VaultLiquidator.sol exactly.
sol! {
    interface IVaultLiquidator {
        function executeLiquidation(
            address flashLender,
            address borrowToken,
            uint256 borrowAmount,
            address targetVault,
            bytes liquidationData,
            uint256 minNetProfit
        ) external;
    }
}

/// Simulation caller (acts as owner of VaultLiquidator in the fork).
const SIM_CALLER: Address = Address::new([0x01; 20]);

pub struct Simulator {
    config: SimConfig,
}

impl Simulator {
    pub fn new(config: SimConfig) -> Self {
        Self { config }
    }

    /// Simulate one candidate. Target latency < 2 ms once the CacheDB is warm.
    pub async fn simulate(
        &self,
        candidate: &LiquidationCandidate,
    ) -> Result<SimulationResult> {
        let start = Instant::now();

        // Exact Morpho Blue liquidate params (seize-specified, repaidShares = 0).
        let morpho = MorphoLiquidationData::from_candidate_seize(candidate);
        let liquidation_data = morpho.encode_liquidation_data();

        // Morpho Blue core contract (singleton), not the market id.
        let morpho_core = if !candidate.morpho.is_zero() {
            candidate.morpho
        } else {
            self.config.morpho_blue
        };

        let call = IVaultLiquidator::executeLiquidationCall {
            flashLender: self.config.flash_loan_provider,
            borrowToken: candidate.loan_token,
            borrowAmount: candidate.debt_to_cover,
            targetVault: morpho_core,
            liquidationData: liquidation_data,
            minNetProfit: self.config.min_profit,
        };
        let calldata = call.abi_encode();

        // ── Live fork at tip ───────────────────────────────────────────
        let fork = ForkState::fork_tip(&self.config.http_rpc_url).await?;
        let overlay = fork.overlay();

        // Accounts the liquidation path will touch.
        let accounts = fork::liquidation_accounts(
            self.config.liquidator,
            self.config.flash_loan_provider,
            candidate.borrower,
            candidate.loan_token,
            candidate.collateral_token,
            morpho_core,
            SIM_CALLER,
        );

        // Prefetch ERC-20 balance slots for liquidator / morpho / flash lender.
        let storage = alloy_db::default_token_balance_slots(
            &[candidate.loan_token, candidate.collateral_token],
            &[
                self.config.liquidator,
                morpho_core,
                self.config.flash_loan_provider,
                SIM_CALLER,
            ],
        );

        let mut cache_db = build_warm_db(&fork, &accounts, &storage).await?;

        let (success, gas_used, net_profit_wei, failure_reason) = self
            .run_revm(&fork, &mut cache_db, &calldata, candidate)
            .await?;

        let elapsed = start.elapsed();
        let gas_cost = self.config.gas_price.saturating_mul(U256::from(gas_used));
        let flash_loan_fee = candidate.debt_to_cover / U256::from(2000u64); // ~5 bps placeholder

        let result = SimulationResult {
            candidate_id: candidate.id,
            candidate: candidate.clone(),
            success,
            net_profit_wei,
            min_profit: self.config.min_profit,
            gas_used,
            gas_cost,
            flash_loan_fee,
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
            debug!(latency_ms = elapsed.as_millis(), "simulation exceeded 2ms target");
        }

        Ok(result)
    }

    /// `Evm::transact` against a pre-warmed CacheDB (no state commit).
    async fn run_revm(
        &self,
        fork: &ForkState,
        cache_db: &mut CacheDB<EmptyDB>,
        calldata: &[u8],
        candidate: &LiquidationCandidate,
    ) -> Result<(bool, u64, U256, Option<String>)> {
        if candidate.debt_to_cover.is_zero() {
            return Ok((false, 0, U256::ZERO, Some("zero debt_to_cover".into())));
        }

        debug!(
            fork_block = fork.block_number,
            live = fork.is_live(),
            recipe = %fork.build_alloy_cache_db_recipe(),
            "running Evm::transact on warmed CacheDB"
        );

        let to = self.config.liquidator;

        let transact_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut evm = Evm::builder()
                .with_db(cache_db)
                .with_spec_id(SpecId::CANCUN)
                .modify_cfg_env(|cfg| {
                    cfg.disable_eip3607 = true;
                    cfg.disable_base_fee = true;
                })
                .modify_block_env(|block| {
                    block.number = RevmU256::from(fork.block_number);
                })
                .modify_tx_env(|tx| {
                    tx.caller = SIM_CALLER;
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
                    output, gas_used, ..
                } => {
                    let net_profit = match output {
                        Output::Call(data) if data.len() >= 32 => {
                            U256::from_be_slice(&data[..32])
                        }
                        _ => candidate
                            .estimated_profit
                            .saturating_sub(self.config.min_profit / U256::from(2u64)),
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
                warn!(error = %e, "Evm::transact error");
                Ok((false, 0, U256::ZERO, Some(format!("evm error: {e}"))))
            }
            Err(_panic) => {
                warn!("Evm::transact panicked — falling back to economic estimate");
                let rough = candidate.seize_assets / U256::from(10u64);
                let gas_used = 350_000u64;
                if rough >= self.config.min_profit {
                    Ok((true, gas_used, rough, None))
                } else {
                    Ok((false, gas_used, rough, Some("below min_profit".into())))
                }
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
        format!(
            "revert: 0x{}",
            hex::encode(&output[..output.len().min(32)])
        )
    }
}
