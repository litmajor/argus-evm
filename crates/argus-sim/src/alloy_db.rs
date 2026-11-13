//! Lazy storage-backed simulation database.
//!
//! Two strategies:
//!
//! 1. **Prefetch + slot warm** (`build_warm_db`): account basics + explicit
//!    storage slots loaded via Alloy HTTP into `CacheDB<EmptyDB>`. Zero RPC
//!    during `Evm::transact`.
//!
//! 2. **Provider-backed lazy DB**: storage misses query the pinned Alloy
//!    provider and are cached by `CacheDB`.
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

use std::future::IntoFuture;

use alloy::eips::BlockId;
use alloy::providers::{Provider, ReqwestProvider};
use alloy_primitives::{Address, B256, U256};
use argus_types::Result;
use revm::db::{CacheDB, EmptyDB};
use revm::primitives::{AccountInfo, Bytecode};
use revm::DatabaseRef;
use tokio::runtime::{Handle, RuntimeFlavor};
use tracing::{debug, info};

use crate::fork::ForkState;

/// Synchronous revm adapter for Alloy 0.9's asynchronous HTTP provider.
///
/// revm 19's built-in AlloyDB uses Alloy 0.7 types, which are incompatible
/// with this workspace's Alloy 0.9 provider. This adapter keeps the provider
/// version aligned and bridges database reads through the active Tokio runtime.
#[derive(Clone)]
pub struct ProviderDb {
    provider: ReqwestProvider,
    block: BlockId,
    runtime: Handle,
}

impl ProviderDb {
    pub fn new(provider: ReqwestProvider, block: BlockId) -> Result<Self> {
        let runtime = Handle::try_current()
            .map_err(|error| argus_types::ArgusError::simulation(error.to_string()))?;
        if runtime.runtime_flavor() != RuntimeFlavor::MultiThread {
            return Err(argus_types::ArgusError::simulation(
                "provider-backed simulation requires a multi-thread Tokio runtime",
            ));
        }

        Ok(Self {
            provider,
            block,
            runtime,
        })
    }

    fn block_on<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        tokio::task::block_in_place(|| self.runtime.block_on(future))
    }
}

impl DatabaseRef for ProviderDb {
    type Error = String;

    fn basic_ref(&self, address: Address) -> std::result::Result<Option<AccountInfo>, Self::Error> {
        let nonce = self
            .provider
            .get_transaction_count(address)
            .block_id(self.block);
        let balance = self.provider.get_balance(address).block_id(self.block);
        let code = self.provider.get_code_at(address).block_id(self.block);
        let (nonce, balance, code) = self.block_on(async {
            tokio::join!(
                nonce.into_future(),
                balance.into_future(),
                code.into_future()
            )
        });

        let nonce = nonce.map_err(|error| error.to_string())?;
        let balance = balance.map_err(|error| error.to_string())?;
        let code = Bytecode::new_raw(code.map_err(|error| error.to_string())?.into());
        let code_hash = code.hash_slow();

        Ok(Some(AccountInfo::new(balance, nonce, code_hash, code)))
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> std::result::Result<Bytecode, Self::Error> {
        Err(format!(
            "code hash {code_hash} was not loaded with its account; refusing uncached code lookup"
        ))
    }

    fn storage_ref(&self, address: Address, index: U256) -> std::result::Result<U256, Self::Error> {
        self.block_on(
            self.provider
                .get_storage_at(address, index)
                .block_id(self.block)
                .into_future(),
        )
        .map_err(|error| error.to_string())
    }

    fn block_hash_ref(&self, number: u64) -> std::result::Result<B256, Self::Error> {
        let block = self
            .block_on(
                self.provider
                    .get_block_by_number(number.into(), false.into()),
            )
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("block {number} was not returned by the provider"))?;
        Ok(block.header.hash)
    }
}

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

/// Documented provider-backed lazy database construction string.
pub fn alloy_lazy_recipe(rpc_url: &str, block: u64) -> String {
    format!(
        "ProviderDb::new(http:{rpc_url}, block={block}) → CacheDB  \
         (storage on demand, cached after first miss)"
    )
}

/// ERC-20 balance storage slot (Solidity mapping(address => uint256) at slot 0).
/// OpenZeppelin balances are usually at slot 0; some tokens use slot 1/51.
pub fn erc20_balance_slot(holder: Address, mapping_slot: U256) -> U256 {
    use alloy_primitives::keccak256;
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
