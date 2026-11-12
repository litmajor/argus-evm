//! State engine: consume DomainEvents → update store → emit candidates.

use std::sync::Arc;

use alloy_primitives::U256;
use argus_types::{
    ArgusError, CandidateId, CandidateTrigger, DomainEvent, HealthFactor,
    LiquidationCandidate, Market, OraclePrice, Position, PositionKey, Protocol, Result,
};
use chrono::Utc;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info, trace, warn};

use crate::health::{compute_health_factor, estimate_repay_and_seize};
use crate::store::StateStore;

pub struct StateEngine {
    store: Arc<StateStore>,
    candidate_tx: mpsc::Sender<LiquidationCandidate>,
}

impl StateEngine {
    pub fn new(
        store: Arc<StateStore>,
        candidate_tx: mpsc::Sender<LiquidationCandidate>,
    ) -> Self {
        Self {
            store,
            candidate_tx,
        }
    }

    pub async fn run(
        self,
        mut event_rx: broadcast::Receiver<DomainEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("state engine shutting down");
                        break;
                    }
                }
                msg = event_rx.recv() => {
                    match msg {
                        Ok(evt) => {
                            if let Err(e) = self.handle_event(evt).await {
                                warn!(error = %e, "state engine handle_event error");
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "state engine lagged on broadcast");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("event channel closed");
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_event(&self, evt: DomainEvent) -> Result<()> {
        match evt {
            DomainEvent::NewHead(e) => {
                self.store.set_latest_block(e.header.number);
                // Periodic full scan on every N blocks (optional)
                if e.header.number % 10 == 0 {
                    self.scan_all_positions(CandidateTrigger::PeriodicScan)
                        .await?;
                }
            }
            DomainEvent::MarketCreated(e) => {
                let market = Market {
                    id: e.market_id,
                    protocol: e.protocol,
                    loan_token: e.loan_token,
                    collateral_token: e.collateral_token,
                    oracle: e.oracle,
                    irm: e.irm,
                    lltv: e.lltv,
                    total_supply_assets: U256::ZERO,
                    total_supply_shares: U256::ZERO,
                    total_borrow_assets: U256::ZERO,
                    total_borrow_shares: U256::ZERO,
                    last_update: e.block_number,
                    fee: U256::ZERO,
                    updated_at: Utc::now(),
                };
                self.store.upsert_market(market);
                info!(market = %e.market_id, "market registered");
            }
            DomainEvent::Borrow(e) => {
                self.apply_borrow(&e).await?;
            }
            DomainEvent::Repay(e) => {
                self.apply_repay(&e).await?;
            }
            DomainEvent::Liquidate(e) => {
                // Position may be closed or reduced by someone else
                let key = PositionKey {
                    market_id: e.market_id,
                    user: e.borrower,
                };
                if let Some(mut pos) = self.store.get_position(&key) {
                    // Conservative: zero out on external liquidation
                    pos.borrow_shares = U256::ZERO;
                    pos.collateral = pos.collateral.saturating_sub(e.seized_assets);
                    pos.health_factor = HealthFactor::ONE;
                    pos.updated_at = Utc::now();
                    pos.last_block = e.block_number;
                    if pos.collateral.is_zero() && pos.borrow_shares.is_zero() {
                        self.store.remove_position(&key);
                    } else {
                        self.store.upsert_position(pos);
                    }
                }
            }
            DomainEvent::OracleUpdate(e) => {
                self.store.set_oracle(OraclePrice {
                    asset: e.asset,
                    price: e.price,
                    updated_at: e.block_number,
                    source: e.protocol,
                });
                // Re-evaluate all positions that use this asset
                self.scan_affected_by_oracle(
                    e.asset,
                    CandidateTrigger::OracleUpdate { asset: e.asset },
                )
                .await?;
            }
            DomainEvent::RawLog(_) => {
                trace!("raw log ignored by state engine");
            }
        }
        Ok(())
    }

    async fn apply_borrow(&self, e: &argus_types::BorrowEvent) -> Result<()> {
        let key = PositionKey {
            market_id: e.market_id,
            user: e.borrower,
        };
        let mut pos = self.store.get_position(&key).unwrap_or(Position {
            market_id: e.market_id,
            protocol: e.protocol,
            user: e.borrower,
            supply_shares: U256::ZERO,
            borrow_shares: U256::ZERO,
            collateral: U256::ZERO,
            health_factor: HealthFactor::ONE,
            updated_at: Utc::now(),
            last_block: e.block_number,
        });
        pos.borrow_shares = pos.borrow_shares.saturating_add(e.shares);
        pos.last_block = e.block_number;
        pos.updated_at = Utc::now();

        // Recompute HF if we have market + oracles
        if let Some(market) = self.store.get_market(&e.market_id) {
            if let (Some(cp), Some(lp)) = (
                self.store.get_oracle(&market.collateral_token),
                self.store.get_oracle(&market.loan_token),
            ) {
                pos.health_factor = compute_health_factor(&pos, &market, &cp, &lp);
            }
        }
        self.store.upsert_position(pos.clone());

        if pos.is_liquidatable() {
            self.emit_candidate(pos, CandidateTrigger::Borrow).await?;
        }
        Ok(())
    }

    async fn apply_repay(&self, e: &argus_types::RepayEvent) -> Result<()> {
        let key = PositionKey {
            market_id: e.market_id,
            user: e.borrower,
        };
        if let Some(mut pos) = self.store.get_position(&key) {
            pos.borrow_shares = pos.borrow_shares.saturating_sub(e.shares);
            pos.last_block = e.block_number;
            pos.updated_at = Utc::now();
            if pos.borrow_shares.is_zero() && pos.collateral.is_zero() {
                self.store.remove_position(&key);
            } else {
                if let Some(market) = self.store.get_market(&e.market_id) {
                    if let (Some(cp), Some(lp)) = (
                        self.store.get_oracle(&market.collateral_token),
                        self.store.get_oracle(&market.loan_token),
                    ) {
                        pos.health_factor = compute_health_factor(&pos, &market, &cp, &lp);
                    }
                }
                self.store.upsert_position(pos);
            }
        }
        Ok(())
    }

    async fn scan_all_positions(&self, trigger: CandidateTrigger) -> Result<()> {
        let positions = self.store.all_positions();
        for pos in positions {
            if pos.is_liquidatable() {
                self.emit_candidate(pos, trigger.clone()).await?;
            }
        }
        Ok(())
    }

    async fn scan_affected_by_oracle(
        &self,
        asset: alloy_primitives::Address,
        trigger: CandidateTrigger,
    ) -> Result<()> {
        // Re-evaluate every position whose market uses this asset
        for market in self.store.markets_iter() {
            if market.loan_token != asset && market.collateral_token != asset {
                continue;
            }
            let positions = self.store.positions_for_market(&market.id);
            for mut pos in positions {
                if let (Some(cp), Some(lp)) = (
                    self.store.get_oracle(&market.collateral_token),
                    self.store.get_oracle(&market.loan_token),
                ) {
                    pos.health_factor = compute_health_factor(&pos, &market, &cp, &lp);
                    pos.updated_at = Utc::now();
                    self.store.upsert_position(pos.clone());
                    if pos.is_liquidatable() {
                        self.emit_candidate(pos, trigger.clone()).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn emit_candidate(
        &self,
        pos: Position,
        trigger: CandidateTrigger,
    ) -> Result<()> {
        let market = self
            .store
            .get_market(&pos.market_id)
            .ok_or_else(|| ArgusError::MarketNotFound(pos.market_id.to_string()))?;

        let (cp, lp) = match (
            self.store.get_oracle(&market.collateral_token),
            self.store.get_oracle(&market.loan_token),
        ) {
            (Some(c), Some(l)) => (c, l),
            _ => {
                debug!("missing oracle for candidate emission");
                return Ok(());
            }
        };

        let (repay, seize) = estimate_repay_and_seize(&pos, &market, &cp, &lp);

        let candidate = LiquidationCandidate {
            id: CandidateId::new(),
            protocol: pos.protocol,
            market_id: pos.market_id,
            borrower: pos.user,
            health_factor: pos.health_factor,
            debt_to_cover: repay,
            seize_assets: seize,
            estimated_profit: seize / alloy_primitives::U256::from(10u64), // rough 10% of seize pre-sim
            loan_token: market.loan_token,
            collateral_token: market.collateral_token,
            oracle: market.oracle,
            irm: market.irm,
            lltv: market.lltv,
            // Morpho Blue singleton — configure per-chain; ZERO until set.
            morpho: alloy_primitives::Address::ZERO,
            detected_at_block: self.store.latest_block(),
            detected_at: Utc::now(),
            trigger,
        };

        info!(
            candidate = %candidate.id.0,
            borrower = %candidate.borrower,
            hf = %candidate.health_factor,
            "liquidation candidate emitted"
        );

        self.candidate_tx
            .send(candidate)
            .await
            .map_err(|_| ArgusError::ChannelClosed)?;
        Ok(())
    }
}
