use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimConfig {
    /// VaultLiquidator contract address (deployed).
    pub liquidator: Address,
    /// Transaction sender used for authorization checks during simulation.
    pub caller: Address,
    /// Morpho Blue singleton (mainnet: 0xBBBB…BBBB).
    pub morpho_blue: Address,
    /// Legacy flash-loan provider setting; unused by the Morpho-native callback flow.
    pub flash_loan_provider: Address,
    /// Minimum net profit (in loan token wei) required to consider profitable.
    pub min_profit: U256,
    /// Assumed L2 gas price (wei) for cost estimation.
    pub gas_price: U256,
    /// Chain id.
    pub chain_id: u64,
    /// HTTP RPC URL used to build the AlloyDB / CacheDB fork.
    pub http_rpc_url: String,
    /// Optional: path to a pre-loaded revm database dump for offline testing.
    pub state_dump_path: Option<String>,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            liquidator: Address::ZERO,
            caller: Address::ZERO,
            // Mainnet Morpho Blue
            morpho_blue: alloy_primitives::address!("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"),
            flash_loan_provider: Address::ZERO,
            min_profit: U256::from(10u64).pow(U256::from(15u64)), // 0.001 ETH-ish
            gas_price: U256::from(100_000_000u64),                // 0.1 gwei
            chain_id: 1,
            http_rpc_url: "https://eth.llamarpc.com".to_string(),
            state_dump_path: None,
        }
    }
}
