//! Concurrent in-memory state store (DashMap + parking_lot).

use alloy_primitives::{Address, U256};
use argus_types::{
    Market, MarketId, OraclePrice, Position, PositionKey, Protocol,
};
use chrono::Utc;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Hot state mirror. All reads/writes are lock-free or fine-grained.
pub struct StateStore {
    markets: DashMap<MarketId, Market>,
    positions: DashMap<PositionKey, Position>,
    /// asset → latest oracle price
    oracles: DashMap<Address, OraclePrice>,
    latest_block: AtomicU64,
}

impl StateStore {
    pub fn new() -> Self {
        Self {
            markets: DashMap::new(),
            positions: DashMap::new(),
            oracles: DashMap::new(),
            latest_block: AtomicU64::new(0),
        }
    }

    pub fn set_latest_block(&self, n: u64) {
        self.latest_block.store(n, Ordering::Release);
    }

    pub fn latest_block(&self) -> u64 {
        self.latest_block.load(Ordering::Acquire)
    }

    // ── Markets ──────────────────────────────────────────────

    pub fn upsert_market(&self, market: Market) {
        self.markets.insert(market.id, market);
    }

    pub fn get_market(&self, id: &MarketId) -> Option<Market> {
        self.markets.get(id).map(|m| m.clone())
    }

    pub fn markets_iter(&self) -> impl Iterator<Item = Market> + '_ {
        self.markets.iter().map(|e| e.value().clone())
    }

    // ── Positions ────────────────────────────────────────────

    pub fn upsert_position(&self, pos: Position) {
        let key = pos.key();
        self.positions.insert(key, pos);
    }

    pub fn get_position(&self, key: &PositionKey) -> Option<Position> {
        self.positions.get(key).map(|p| p.clone())
    }

    pub fn remove_position(&self, key: &PositionKey) {
        self.positions.remove(key);
    }

    pub fn positions_for_market(&self, market_id: &MarketId) -> Vec<Position> {
        self.positions
            .iter()
            .filter(|e| e.key().market_id == *market_id)
            .map(|e| e.value().clone())
            .collect()
    }

    pub fn all_positions(&self) -> Vec<Position> {
        self.positions.iter().map(|e| e.value().clone()).collect()
    }

    pub fn position_count(&self) -> usize {
        self.positions.len()
    }

    // ── Oracles ──────────────────────────────────────────────

    pub fn set_oracle(&self, price: OraclePrice) {
        self.oracles.insert(price.asset, price);
    }

    pub fn get_oracle(&self, asset: &Address) -> Option<OraclePrice> {
        self.oracles.get(asset).map(|p| p.clone())
    }
}

impl Default for StateStore {
    fn default() -> Self {
        Self::new()
    }
}
