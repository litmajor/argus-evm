//! Structured JSONL telemetry records for high-frequency debugging.
//!
//! Every hot-path tick (block, sim, execution) emits one of these records.
//! Designed to be written with `serde_json::to_writer` + newline for
//! zero-allocation-friendly append-only logs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::candidate::CandidateId;

/// Top-level telemetry envelope. One JSON object per line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TelemetryRecord {
    Block(BlockTick),
    Candidate(CandidateTick),
    Simulation(SimTick),
    Execution(ExecTick),
    Error(ErrorTick),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockTick {
    pub ts: DateTime<Utc>,
    pub block_number: u64,
    pub block_hash: String,
    pub lag_ms: u64,
    pub positions_evaluated: u32,
    pub candidates_emitted: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateTick {
    pub ts: DateTime<Utc>,
    pub candidate_id: CandidateId,
    pub market_id: String,
    pub borrower: String,
    pub health_factor: String,
    pub trigger: String,
    pub detected_at_block: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimTick {
    pub ts: DateTime<Utc>,
    pub candidate_id: CandidateId,
    pub success: bool,
    pub net_profit: String,
    pub gas_used: u64,
    pub sim_latency_us: u64,
    pub fork_block: u64,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecTick {
    pub ts: DateTime<Utc>,
    pub candidate_id: CandidateId,
    pub success: bool,
    pub tx_hash: Option<String>,
    pub error: Option<String>,
    pub submit_latency_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorTick {
    pub ts: DateTime<Utc>,
    pub component: String,
    pub message: String,
    pub correlation_id: Option<Uuid>,
}
