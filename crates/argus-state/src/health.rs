//! Health-factor computation with **exact Morpho Blue** math.
//!
//! ## SharesMathLib
//! Source: morpho-blue/src/libraries/SharesMathLib.sol
//!   VIRTUAL_SHARES = 1e6, VIRTUAL_ASSETS = 1
//!
//! ## Liquidation Incentive Factor (LIF)
//! Source: https://docs.morpho.org/morpho/concepts/liquidation
//!
//!   LIF = min(M, 1 / (β·LLTV + (1−β)))
//!   β = 0.3,  M = 1.15
//!
//! Morpho Blue has **no close factor** — a liquidator may repay up to 100% of
//! debt in one transaction. The entire LIF bonus goes to the liquidator.

use alloy_primitives::{Address, U256};
use argus_types::{HealthFactor, Market, OraclePrice, Position};

/// Morpho Blue virtual offsets (SharesMathLib).
pub const VIRTUAL_SHARES: u64 = 1_000_000;
pub const VIRTUAL_ASSETS: u64 = 1;

/// LIF curve parameters (WAD = 1e18).
pub const LIF_BETA_WAD: u64 = 300_000_000_000_000_000; // 0.3e18
pub const LIF_MAX_WAD: u64 = 1_150_000_000_000_000_000; // 1.15e18
pub const WAD: u64 = 1_000_000_000_000_000_000; // 1e18

// ─────────────────────────────────────────────────────────────
// Fixed-point mulDiv
// ─────────────────────────────────────────────────────────────

#[inline]
pub fn mul_div_down(x: U256, y: U256, d: U256) -> U256 {
    if d.is_zero() {
        return U256::ZERO;
    }
    x.saturating_mul(y).checked_div(d).unwrap_or(U256::ZERO)
}

#[inline]
pub fn mul_div_up(x: U256, y: U256, d: U256) -> U256 {
    if d.is_zero() {
        return U256::ZERO;
    }
    let prod = x.saturating_mul(y);
    let numer = prod.saturating_add(d.saturating_sub(U256::from(1u64)));
    numer.checked_div(d).unwrap_or(U256::ZERO)
}

// ─────────────────────────────────────────────────────────────
// SharesMathLib
// ─────────────────────────────────────────────────────────────

pub fn to_assets_down(shares: U256, total_assets: U256, total_shares: U256) -> U256 {
    mul_div_down(
        shares,
        total_assets.saturating_add(U256::from(VIRTUAL_ASSETS)),
        total_shares.saturating_add(U256::from(VIRTUAL_SHARES)),
    )
}

pub fn to_assets_up(shares: U256, total_assets: U256, total_shares: U256) -> U256 {
    mul_div_up(
        shares,
        total_assets.saturating_add(U256::from(VIRTUAL_ASSETS)),
        total_shares.saturating_add(U256::from(VIRTUAL_SHARES)),
    )
}

pub fn to_shares_down(assets: U256, total_assets: U256, total_shares: U256) -> U256 {
    mul_div_down(
        assets,
        total_shares.saturating_add(U256::from(VIRTUAL_SHARES)),
        total_assets.saturating_add(U256::from(VIRTUAL_ASSETS)),
    )
}

pub fn to_shares_up(assets: U256, total_assets: U256, total_shares: U256) -> U256 {
    mul_div_up(
        assets,
        total_shares.saturating_add(U256::from(VIRTUAL_SHARES)),
        total_assets.saturating_add(U256::from(VIRTUAL_ASSETS)),
    )
}

/// Borrow shares → debt assets (always round **up** against the borrower).
pub fn borrow_shares_to_assets(
    borrow_shares: U256,
    total_borrow_assets: U256,
    total_borrow_shares: U256,
) -> U256 {
    to_assets_up(borrow_shares, total_borrow_assets, total_borrow_shares)
}

pub fn supply_shares_to_assets(
    supply_shares: U256,
    total_supply_assets: U256,
    total_supply_shares: U256,
) -> U256 {
    to_assets_down(supply_shares, total_supply_assets, total_supply_shares)
}

// ─────────────────────────────────────────────────────────────
// Liquidation Incentive Factor (exact Morpho formula)
// ─────────────────────────────────────────────────────────────

/// Compute LIF in WAD (1e18 = 1.0) from market LLTV (also WAD).
///
/// ```text
/// LIF = min(1.15e18, 1e36 / (β·LLTV/1e18 + (1−β)))
///     = min(1.15e18, 1e36 / (β·LLTV + (1−β)·1e18) * 1e18 / 1e18)
/// ```
///
/// With β = 0.3:
///   denom = 0.3·LLTV + 0.7·1e18   (still in WAD units)
///   LIF   = min(1.15e18, 1e36 / denom)
pub fn liquidation_incentive_factor(lltv_wad: U256) -> U256 {
    let beta = U256::from(LIF_BETA_WAD);
    let one = U256::from(WAD);
    let max_lif = U256::from(LIF_MAX_WAD);
    let one_minus_beta = one.saturating_sub(beta); // 0.7e18

    // denom = β * LLTV / 1e18 + (1−β)
    //      = (β * LLTV + (1−β) * 1e18) / 1e18
    // We keep denom_scaled = β * LLTV + (1−β) * 1e18  (WAD² scale for first term)
    let denom = mul_div_down(beta, lltv_wad, one).saturating_add(one_minus_beta);

    if denom.is_zero() {
        return max_lif;
    }

    // denom is in WAD (e.g. 0.958e18). LIF = 1 / (denom/1e18) = 1e18/denom
    // = mul_div_down(1e18, 1e18, denom) = 1e36/denom ✓
    let lif = mul_div_down(one, one, denom);

    if lif > max_lif {
        max_lif
    } else {
        lif
    }
}

// ─────────────────────────────────────────────────────────────
// Health factor
// ─────────────────────────────────────────────────────────────

/// HF = (collateral_value × LLTV) / borrow_value   (all 1e18 fixed-point)
pub fn compute_health_factor(
    position: &Position,
    market: &Market,
    collateral_price: &OraclePrice,
    loan_price: &OraclePrice,
) -> HealthFactor {
    if position.borrow_shares.is_zero() {
        return HealthFactor::ONE;
    }

    let borrow_assets = borrow_shares_to_assets(
        position.borrow_shares,
        market.total_borrow_assets,
        market.total_borrow_shares,
    );

    let wads = U256::from(WAD);

    let coll_value = position
        .collateral
        .saturating_mul(collateral_price.price)
        .checked_div(wads)
        .unwrap_or(U256::ZERO);

    let borrow_value = borrow_assets
        .saturating_mul(loan_price.price)
        .checked_div(wads)
        .unwrap_or(U256::ZERO);

    if borrow_value.is_zero() {
        return HealthFactor::ONE;
    }

    let numerator = coll_value.saturating_mul(market.lltv);
    let hf_raw = numerator.checked_div(borrow_value).unwrap_or(U256::ZERO);

    HealthFactor::from_raw(hf_raw)
}

// ─────────────────────────────────────────────────────────────
// Liquidation sizing (no close factor — up to 100% of debt)
// ─────────────────────────────────────────────────────────────

/// Returns `(repay_assets, seize_collateral_assets)`.
///
/// Morpho Blue: liquidator may repay **any amount up to 100%** of debt.
/// We default to full repayment (`debt_to_cover = borrow_assets`).
///
/// Seized collateral (in collateral token units):
///   seize = repay_value_in_collateral × LIF
///        = repay × loan_price / collateral_price × LIF / 1e18
pub fn estimate_repay_and_seize(
    position: &Position,
    market: &Market,
    collateral_price: &OraclePrice,
    loan_price: &OraclePrice,
) -> (U256, U256) {
    let borrow_assets = borrow_shares_to_assets(
        position.borrow_shares,
        market.total_borrow_assets,
        market.total_borrow_shares,
    );

    if borrow_assets.is_zero() {
        return (U256::ZERO, U256::ZERO);
    }

    // No close factor — full debt is available to cover.
    let repay = borrow_assets;

    let lif = liquidation_incentive_factor(market.lltv);
    let wads = U256::from(WAD);

    let seize = if collateral_price.price.is_zero() {
        U256::ZERO
    } else {
        // seize = repay * loan_price * LIF / (collateral_price * 1e18)
        repay
            .saturating_mul(loan_price.price)
            .saturating_mul(lif)
            .checked_div(collateral_price.price.saturating_mul(wads))
            .unwrap_or(U256::ZERO)
    };

    (repay, seize.min(position.collateral))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_market_one_asset_is_one_million_shares() {
        let shares = to_shares_down(U256::from(1u64), U256::ZERO, U256::ZERO);
        assert_eq!(shares, U256::from(VIRTUAL_SHARES));
    }

    #[test]
    fn to_assets_up_rounds_against_borrower() {
        let down = to_assets_down(U256::from(1u64), U256::from(100u64), U256::from(100u64));
        let up = to_assets_up(U256::from(1u64), U256::from(100u64), U256::from(100u64));
        assert!(up >= down);
    }

    #[test]
    fn lif_at_86_percent_lltv() {
        // LLTV = 0.86e18 → LIF ≈ 1.0438e18
        let lltv = U256::from(860_000_000_000_000_000u64);
        let lif = liquidation_incentive_factor(lltv);
        // 1.04e18 < lif < 1.05e18
        assert!(lif > U256::from(1_040_000_000_000_000_000u64));
        assert!(lif < U256::from(1_050_000_000_000_000_000u64));
    }

    #[test]
    fn lif_capped_at_115_percent() {
        // Very low LLTV → formula would exceed 1.15, must cap
        let lltv = U256::from(100_000_000_000_000_000u64); // 0.1
        let lif = liquidation_incentive_factor(lltv);
        assert_eq!(lif, U256::from(LIF_MAX_WAD));
    }

    #[test]
    fn lif_at_98_percent_is_small() {
        // LLTV = 0.98e18 → LIF ≈ 1.006
        let lltv = U256::from(980_000_000_000_000_000u64);
        let lif = liquidation_incentive_factor(lltv);
        assert!(lif > U256::from(WAD)); // > 1.0
        assert!(lif < U256::from(1_010_000_000_000_000_000u64)); // < 1.01
    }
}
