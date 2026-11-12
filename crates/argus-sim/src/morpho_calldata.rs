//! Exact Morpho Blue `liquidate` calldata encoding.
//!
//! ```solidity
//! struct MarketParams {
//!     address loanToken;
//!     address collateralToken;
//!     address oracle;
//!     address irm;
//!     uint256 lltv;
//! }
//!
//! function liquidate(
//!     MarketParams memory marketParams,
//!     address borrower,
//!     uint256 seizedAssets,
//!     uint256 repaidShares,
//!     bytes memory data
//! ) external returns (uint256 assetsSeized, uint256 assetsRepaid);
//! ```
//!
//! Rule: **exactly one** of `seizedAssets` / `repaidShares` must be zero.
//! We default to seizedAssets > 0, repaidShares = 0 (seize-specified liquidation).

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_sol_types::{sol, SolCall, SolValue};
use argus_types::LiquidationCandidate;

sol! {
    /// Morpho Blue market parameters (id = keccak256(abi.encode(params))).
    struct MarketParams {
        address loanToken;
        address collateralToken;
        address oracle;
        address irm;
        uint256 lltv;
    }

    interface IMorpho {
        function liquidate(
            MarketParams marketParams,
            address borrower,
            uint256 seizedAssets,
            uint256 repaidShares,
            bytes data
        ) external returns (uint256, uint256);
    }
}

/// Payload embedded in `VaultLiquidator.liquidationData`.
/// Decoded on-chain and passed straight into `IMorpho.liquidate`.
#[derive(Clone, Debug)]
pub struct MorphoLiquidationData {
    pub market_params: MarketParams,
    pub borrower: Address,
    pub seized_assets: U256,
    pub repaid_shares: U256,
    /// Callback data for `onMorphoLiquidate` — empty when unused.
    pub data: Bytes,
}

impl MorphoLiquidationData {
    /// Build from a candidate. Seize-specified path (repaidShares = 0).
    pub fn from_candidate_seize(c: &LiquidationCandidate) -> Self {
        Self {
            market_params: MarketParams {
                loanToken: c.loan_token,
                collateralToken: c.collateral_token,
                oracle: c.oracle,
                irm: c.irm,
                lltv: c.lltv,
            },
            borrower: c.borrower,
            seized_assets: c.seize_assets,
            repaid_shares: U256::ZERO,
            data: Bytes::new(),
        }
    }

    /// Build from a candidate. Share-specified path (seizedAssets = 0).
    /// `repaid_shares` should already be converted via SharesMathLib `toSharesUp`.
    pub fn from_candidate_shares(c: &LiquidationCandidate, repaid_shares: U256) -> Self {
        Self {
            market_params: MarketParams {
                loanToken: c.loan_token,
                collateralToken: c.collateral_token,
                oracle: c.oracle,
                irm: c.irm,
                lltv: c.lltv,
            },
            borrower: c.borrower,
            seized_assets: U256::ZERO,
            repaid_shares,
            data: Bytes::new(),
        }
    }

    /// Market id = keccak256(abi.encode(MarketParams)).
    pub fn market_id(&self) -> B256 {
        let encoded = self.market_params.abi_encode();
        keccak256(encoded)
    }

    /// ABI-encode for `VaultLiquidator.liquidationData`:
    /// `(MarketParams, address borrower, uint256 seized, uint256 repaidShares, bytes data)`
    pub fn encode_liquidation_data(&self) -> Bytes {
        (
            self.market_params.clone(),
            self.borrower,
            self.seized_assets,
            self.repaid_shares,
            self.data.clone(),
        )
            .abi_encode()
            .into()
    }

    /// Full calldata for a direct `IMorpho.liquidate` call (for sim / testing).
    pub fn encode_morpho_call(&self) -> Bytes {
        let call = IMorpho::liquidateCall {
            marketParams: self.market_params.clone(),
            borrower: self.borrower,
            seizedAssets: self.seized_assets,
            repaidShares: self.repaid_shares,
            data: self.data.clone(),
        };
        call.abi_encode().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn market_id_is_32_bytes() {
        let params = MarketParams {
            loanToken: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
            collateralToken: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
            oracle: Address::ZERO,
            irm: Address::ZERO,
            lltv: U256::from(860_000_000_000_000_000u64),
        };
        let id = keccak256(params.abi_encode());
        assert_eq!(id.as_slice().len(), 32);
    }
}
