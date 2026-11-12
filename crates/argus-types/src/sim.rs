//! Simulation results produced by the revm-based simulator.

use alloy_primitives::{Address, B256, U256};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::candidate::{CandidateId, LiquidationCandidate};

/// Outcome of a single in-memory revm simulation of the full atomic path:
/// flash-loan → liquidate → DEX swap → repay flash-loan → profit check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub candidate_id: CandidateId,
    pub candidate: LiquidationCandidate,
    pub success: bool,
    /// Net profit in the flash-loan / base token after fees + gas estimate.
    pub net_profit_wei: U256,
    /// Minimum profit threshold that was required.
    pub min_profit: U256,
    pub gas_used: u64,
    /// Estimated L2 gas cost in base-token units.
    pub gas_cost: U256,
    pub flash_loan_fee: U256,
    /// Calldata for `VaultLiquidator.executeLiquidation(...)`.
    pub calldata: Vec<u8>,
    /// Target contract (VaultLiquidator address).
    pub target: Address,
    /// Block number the simulation was forked from.
    pub fork_block: u64,
    pub simulated_at: DateTime<Utc>,
    /// Human-readable reason if success == false.
    pub failure_reason: Option<String>,
}

impl SimulationResult {
    pub fn is_profitable(&self) -> bool {
        self.success && self.net_profit_wei >= self.min_profit
    }
}

/// Bundle ready for submission (Flashbots / private RPC).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionBundle {
    pub candidate_id: CandidateId,
    pub target: Address,
    pub calldata: Vec<u8>,
    pub value: U256,
    pub max_priority_fee_per_gas: U256,
    pub max_fee_per_gas: U256,
    pub gas_limit: u64,
    pub chain_id: u64,
    pub builder_payment: Option<U256>,
    pub simulation: SimulationResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionOutcome {
    pub candidate_id: CandidateId,
    pub success: bool,
    pub tx_hash: Option<B256>,
    pub block_number: Option<u64>,
    pub error: Option<String>,
    pub submitted_at: DateTime<Utc>,
    pub confirmed_at: Option<DateTime<Utc>>,
}
