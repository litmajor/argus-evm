//! Live CacheDB fork via Alloy HTTP state reads.
//!
//! Prefetches account balance / nonce / code at a target block into
//! `CacheDB<EmptyDB>` so `Evm::transact` runs fully in-memory with zero
//! RPC on the hot path.
//!
//! For storage-slot-heavy contracts (Morpho, ERC20 balances), extend
//! `preload_storage` or switch the backing DB to `AlloyDB` +
//! `WrapDatabaseAsync` (lazy RPC on cache miss).

use std::sync::Arc;

use alloy::eips::BlockId;
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use argus_types::{ArgusError, Result};
use revm::database::{CacheDB, EmptyDB};
use revm::primitives::{AccountInfo, Bytecode, KECCAK_EMPTY};
use tracing::{debug, info, warn};

/// Handle to a forked tip. Holds block metadata + live HTTP provider.
#[derive(Clone)]
pub struct ForkState {
    pub block_number: u64,
    pub block_hash: B256,
    pub rpc_url: String,
    provider: Option<RootProvider>,
}

impl Default for ForkState {
    fn default() -> Self {
        Self {
            block_number: 0,
            block_hash: B256::ZERO,
            rpc_url: String::new(),
            provider: None,
        }
    }
}

impl ForkState {
    pub fn new(block_number: u64, block_hash: B256) -> Self {
        Self {
            block_number,
            block_hash,
            rpc_url: String::new(),
            provider: None,
        }
    }

    /// Connect to HTTP RPC and pin tip block number/hash.
    pub async fn fork_tip(rpc_url: &str) -> Result<Self> {
        if rpc_url.is_empty() {
            debug!("fork_tip: empty rpc_url — offline EmptyDB mode");
            return Ok(Self::default());
        }

        let provider = ProviderBuilder::new().connect_http(
            rpc_url
                .parse()
                .map_err(|e| ArgusError::provider(format!("invalid rpc url: {e}")))?,
        );

        let block_number = match provider.get_block_number().await {
            Ok(n) => n,
            Err(e) => {
                debug!(error = %e, "fork_tip: RPC unreachable — offline mode");
                return Ok(Self {
                    block_number: 0,
                    block_hash: B256::ZERO,
                    rpc_url: rpc_url.to_string(),
                    provider: None,
                });
            }
        };

        let block_hash = provider
            .get_block_by_number(block_number.into())
            .await
            .ok()
            .flatten()
            .map(|b| b.header.hash)
            .unwrap_or(B256::ZERO);

        info!(block_number, %block_hash, "forked tip via Alloy HTTP");

        Ok(Self {
            block_number,
            block_hash,
            rpc_url: rpc_url.to_string(),
            provider: Some(provider),
        })
    }

    pub fn is_live(&self) -> bool {
        self.provider.is_some()
    }

    pub fn provider(&self) -> Option<&RootProvider> {
        self.provider.as_ref()
    }

    /// Prefetch account basics (balance, nonce, code) into a warm CacheDB.
    ///
    /// After this returns, `Evm::transact` hits memory for those accounts.
    /// Storage slots are **not** prefetched here — add via `insert_account_storage`
    /// or back the DB with AlloyDB for lazy slot loads.
    pub async fn create_forked_db(
        &self,
        accounts_to_load: &[Address],
    ) -> Result<CacheDB<EmptyDB>> {
        let mut db = CacheDB::new(EmptyDB::default());

        let Some(provider) = &self.provider else {
            debug!("create_forked_db: no provider — empty CacheDB");
            return Ok(db);
        };

        let block = BlockId::Number(self.block_number.into());
        let mut loaded = 0u32;

        for &address in accounts_to_load {
            if address.is_zero() {
                continue;
            }

            match load_account(provider, address, block).await {
                Ok(info) => {
                    db.insert_account_info(address, info);
                    loaded += 1;
                }
                Err(e) => {
                    warn!(%address, error = %e, "failed to load account into CacheDB");
                }
            }
        }

        info!(
            block = self.block_number,
            accounts = loaded,
            requested = accounts_to_load.len(),
            "CacheDB warmed from Alloy"
        );

        Ok(db)
    }

    /// Prefetch a set of storage slots for an account already in the CacheDB.
    pub async fn preload_storage(
        &self,
        db: &mut CacheDB<EmptyDB>,
        address: Address,
        slots: &[U256],
    ) -> Result<()> {
        let Some(provider) = &self.provider else {
            return Ok(());
        };

        let block = BlockId::Number(self.block_number.into());

        for &slot in slots {
            let value = provider
                .get_storage_at(address, slot)
                .block_id(block)
                .await
                .map_err(ArgusError::provider)?;

            db.insert_account_storage(address, slot, value)
                .map_err(|e| ArgusError::revm(format!("insert storage: {e:?}")))?;
        }

        Ok(())
    }

    pub fn build_alloy_cache_db_recipe(&self) -> String {
        format!(
            "create_forked_db @ block {} → CacheDB (prefetched accounts) | optional AlloyDB lazy backend",
            self.block_number
        )
    }

    pub fn overlay(&self) -> ForkOverlay {
        ForkOverlay {
            block_number: self.block_number,
            block_hash: self.block_hash,
            live: self.is_live(),
        }
    }
}

/// Per-simulation overlay metadata (cheap to clone).
#[derive(Debug, Clone)]
pub struct ForkOverlay {
    pub block_number: u64,
    pub block_hash: B256,
    pub live: bool,
}

/// Fetch balance + nonce + code for one account at `block`.
pub async fn load_account(
    provider: &RootProvider,
    address: Address,
    block: BlockId,
) -> Result<AccountInfo> {
    let balance = provider
        .get_balance(address)
        .block_id(block)
        .await
        .map_err(ArgusError::provider)?;

    let nonce = provider
        .get_transaction_count(address)
        .block_id(block)
        .await
        .map_err(ArgusError::provider)?;

    let code_bytes: Bytes = provider
        .get_code_at(address)
        .block_id(block)
        .await
        .map_err(ArgusError::provider)?;

    let (code_hash, code) = if code_bytes.is_empty() {
        (KECCAK_EMPTY, None)
    } else {
        let hash = keccak256(&code_bytes);
        (hash, Some(Bytecode::new_raw(code_bytes)))
    };

    Ok(AccountInfo {
        balance,
        nonce,
        code_hash,
        code,
        ..Default::default()
    })
}

/// Standard account set for a liquidation sim.
pub fn liquidation_accounts(
    liquidator: Address,
    flash_lender: Address,
    borrower: Address,
    loan_token: Address,
    collateral_token: Address,
    vault: Address,
    caller: Address,
) -> Vec<Address> {
    let mut addrs = vec![
        liquidator,
        flash_lender,
        borrower,
        loan_token,
        collateral_token,
        vault,
        caller,
    ];
    addrs.sort();
    addrs.dedup();
    addrs.retain(|a| !a.is_zero());
    addrs
}
