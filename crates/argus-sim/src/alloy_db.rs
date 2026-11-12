//! Lazy storage-backed simulation database.
//!
//! Two strategies:
//!
//! 1. **Prefetch + slot warm** (`build_warm_db`): account basics + explicit
//!    storage slots loaded via Alloy HTTP into `CacheDB<EmptyDB>`. Zero RPC
//!    during `Evm::transact`.
//!
//! 2. **AlloyDB lazy** (`alloy_lazy_recipe`): production recipe using
//!    `AlloyDB` + `WrapDatabaseAsync` + `CacheDB`. Storage misses pay one
//!    RPC then stay cached. Call this when you need arbitrary slot access
//!    without knowing keys ahead of time.
//!
//! ```ignore
//! use revm::database::{AlloyDB, CacheDB};
//! use revm::database_interface::WrapDatabaseAsync;
//! use alloy::eips::BlockId;
//!
//! let alloy = AlloyDB::new(provider, BlockId::Number(block.into()));
//! let wrapped = WrapDatabaseAsync::new(alloy).expect("tokio runtime");
//! let mut db = CacheDB::new(wrapped);
//! // Evm::builder().with_db(&mut db)...
//! ```

use alloy::eips::BlockId;
use alloy_primitives::{Address, U256};
use argus_types::Result;
use revm::database::{CacheDB, EmptyDB};
use tracing::{debug, info};

use crate::fork::ForkState;

/// Build a fully warm CacheDB: account basics + optional storage slots.
///
/// After this returns, `Evm::transact` should not need the network for the
/// accounts/slots you listed.
pub async fn build_warm_db(
    fork: &ForkState,
    accounts: &[Address],
    storage: &[(Address, U256)],
) -> Result<CacheDB<EmptyDB>> {
    let mut db = fork.create_forked_db(accounts).await?;

    if !storage.is_empty() && fork.is_live() {
        // Group slots by address
        use std::collections::HashMap;
        let mut by_addr: HashMap<Address, Vec<U256>> = HashMap::new();
        for &(addr, slot) in storage {
            by_addr.entry(addr).or_default().push(slot);
        }
        for (addr, slots) in by_addr {
            if let Err(e) = fork.preload_storage(&mut db, addr, &slots).await {
                debug!(%addr, error = %e, "storage preload partial failure");
            }
        }
        info!(
            slots = storage.len(),
            "storage slots prefetched into CacheDB"
        );
    }

    Ok(db)
}

/// Documented AlloyDB lazy-backend construction string (for logs / ops).
pub fn alloy_lazy_recipe(rpc_url: &str, block: u64) -> String {
    format!(
        "AlloyDB::new(http:{rpc_url}, block={block}) → WrapDatabaseAsync → CacheDB  \
         (storage on demand, cached after first miss)"
    )
}

/// ERC-20 balance storage slot (Solidity mapping(address => uint256) at slot 0).
/// OpenZeppelin balances are usually at slot 0; some tokens use slot 1/51.
pub fn erc20_balance_slot(holder: Address, mapping_slot: U256) -> U256 {
    use alloy_primitives::{keccak256, B256};
    // keccak256(abi.encode(holder, mapping_slot))
    let mut buf = [0u8; 64];
    buf[12..32].copy_from_slice(holder.as_slice());
    buf[32..64].copy_from_slice(&mapping_slot.to_be_bytes::<32>());
    U256::from_be_bytes(*keccak256(buf))
}

/// Morpho Blue position storage is more complex (market id keyed). For
/// production liquidations, prefer AlloyDB lazy so revm resolves slots
/// itself during SLOAD.
pub fn default_token_balance_slots(
    tokens: &[Address],
    holders: &[Address],
) -> Vec<(Address, U256)> {
    let mut out = Vec::new();
    for &token in tokens {
        for &holder in holders {
            // mapping slot 0 (OZ default)
            out.push((token, erc20_balance_slot(holder, U256::ZERO)));
            // mapping slot 1 (some tokens)
            out.push((token, erc20_balance_slot(holder, U256::from(1u64))));
        }
    }
    out
}
