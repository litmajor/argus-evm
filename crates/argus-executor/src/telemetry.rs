//! Append-only JSONL telemetry writer.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use argus_types::{
    ArgusError, ExecTick, ExecutionOutcome, Result, SimTick, SimulationResult, TelemetryRecord,
};
use chrono::Utc;
use parking_lot::Mutex;
use tracing::error;

pub struct JsonlTelemetry {
    file: Mutex<File>,
}

impl JsonlTelemetry {
    pub fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    pub fn write(&self, record: &TelemetryRecord) {
        let mut f = self.file.lock();
        if let Ok(line) = serde_json::to_string(record) {
            if let Err(e) = writeln!(f, "{line}") {
                error!(error = %e, "telemetry write failed");
            }
        }
    }

    pub fn log_sim_skip(&self, sim: &SimulationResult) {
        self.write(&TelemetryRecord::Simulation(SimTick {
            ts: Utc::now(),
            candidate_id: sim.candidate_id,
            success: false,
            net_profit: sim.net_profit_wei.to_string(),
            gas_used: sim.gas_used,
            sim_latency_us: 0,
            fork_block: sim.fork_block,
            failure_reason: sim.failure_reason.clone(),
        }));
    }

    pub fn log_execution(&self, outcome: &ExecutionOutcome, sim: &SimulationResult) {
        self.write(&TelemetryRecord::Execution(ExecTick {
            ts: Utc::now(),
            candidate_id: outcome.candidate_id,
            success: outcome.success,
            tx_hash: outcome.tx_hash.map(|h| format!("{h:#x}")),
            error: outcome.error.clone(),
            submit_latency_us: 0,
        }));
        // Also log the sim that led to this execution
        self.write(&TelemetryRecord::Simulation(SimTick {
            ts: sim.simulated_at,
            candidate_id: sim.candidate_id,
            success: sim.success,
            net_profit: sim.net_profit_wei.to_string(),
            gas_used: sim.gas_used,
            sim_latency_us: 0,
            fork_block: sim.fork_block,
            failure_reason: sim.failure_reason.clone(),
        }));
    }

    pub fn log_error(&self, component: &str, message: &str) {
        self.write(&TelemetryRecord::Error(argus_types::ErrorTick {
            ts: Utc::now(),
            component: component.to_string(),
            message: message.to_string(),
            correlation_id: None,
        }));
    }
}
