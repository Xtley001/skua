// crates/simulation/src/profit.rs
//
// Net profit functions. ALL parameters are live — never cached across blocks.
// Returns f64::NEG_INFINITY on invalid inputs so the GSS treats them as non-viable.
//
// Gas cost is ALWAYS subtracted. base_fee == 0 is a bug; propagate the error
// upstream rather than computing with a zero base fee.

use crate::amm::AmmPool;
use skua_core::gas::gas_cost_usd;

/// Net profit for a two-leg arb:
///   borrow x (token A) → swap A→B in pool_a → swap B→A in pool_b → repay x + fee
///
/// All parameters must be populated from live chain state.
pub fn net_profit_two_leg(
    pool_a:         &AmmPool, // buy pool (A → B)
    pool_b:         &AmmPool, // sell pool (B → A)
    x:              f64,      // borrow amount in token A USD-equivalent
    flash_fee_bps:  f64,      // from BotState — never hardcoded
    gas_units:      u64,      // profiled estimate for this strategy
    base_fee_wei:   u128,     // from current block header — never zero
    hype_price_usd: f64,      // from HyperCore oraclePx precompile — never zero
) -> f64 {
    if x <= 0.0 {
        return f64::NEG_INFINITY;
    }

    let mid = pool_a.get_output(x);
    if mid <= 0.0 {
        return f64::NEG_INFINITY;
    }

    let out = pool_b.get_output(mid);
    if out <= 0.0 {
        return f64::NEG_INFINITY;
    }

    let flash_fee = x * flash_fee_bps / 10_000.0;
    let gas_usd   = gas_cost_usd(base_fee_wei, gas_units, hype_price_usd);

    out - x - flash_fee - gas_usd
}

/// Net profit for a three-leg triangular arb:
///   borrow x (A) → A→B → B→C → C→A → repay x + fee
pub fn net_profit_three_leg(
    pool_ab:        &AmmPool, // A → B
    pool_bc:        &AmmPool, // B → C
    pool_ca:        &AmmPool, // C → A
    x:              f64,
    flash_fee_bps:  f64,
    gas_units:      u64,
    base_fee_wei:   u128,
    hype_price_usd: f64,
) -> f64 {
    if x <= 0.0 {
        return f64::NEG_INFINITY;
    }

    let ab = pool_ab.get_output(x);
    if ab <= 0.0 { return f64::NEG_INFINITY; }

    let bc = pool_bc.get_output(ab);
    if bc <= 0.0 { return f64::NEG_INFINITY; }

    let ca = pool_ca.get_output(bc);
    if ca <= 0.0 { return f64::NEG_INFINITY; }

    let flash_fee = x * flash_fee_bps / 10_000.0;
    let gas_usd   = gas_cost_usd(base_fee_wei, gas_units, hype_price_usd);

    ca - x - flash_fee - gas_usd
}

/// Net profit for a stablecoin depeg arb (two-leg, stable pool aware):
///   borrow x (stable_a) → buy depegged stable_b at discount → sell back
///   Uses stable pool AMM math for output calculation.
pub fn net_profit_stable_depeg(
    pool_buy:       &AmmPool, // buy the depegged stable (stable pool)
    pool_sell:      &AmmPool, // sell it back at peg
    x:              f64,
    flash_fee_bps:  f64,
    gas_units:      u64,
    base_fee_wei:   u128,
    hype_price_usd: f64,
) -> f64 {
    if x <= 0.0 {
        return f64::NEG_INFINITY;
    }

    let bought = pool_buy.get_output_stable(x);
    if bought <= 0.0 { return f64::NEG_INFINITY; }

    let out = pool_sell.get_output_stable(bought);
    if out <= 0.0 { return f64::NEG_INFINITY; }

    let flash_fee = x * flash_fee_bps / 10_000.0;
    let gas_usd   = gas_cost_usd(base_fee_wei, gas_units, hype_price_usd);

    out - x - flash_fee - gas_usd
}

/// Minimum profit required before submitting a transaction.
/// The floor is the larger of: the operator-configured minimum, or gas × ROI multiplier.
/// Gas cost is always computed from live base_fee — never treated as zero.
pub fn effective_min_profit_usd(
    strategy_min_profit_usd: f64,
    gas_roi_multiplier:      f64,
    gas_units:               u64,
    base_fee_wei:            u128,
    hype_price_usd:          f64,
) -> f64 {
    let gas_usd = gas_cost_usd(base_fee_wei, gas_units, hype_price_usd);
    strategy_min_profit_usd.max(gas_roi_multiplier * gas_usd)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amm::AmmPool;

    fn make_pool(r_in: f64, r_out: f64, fee_bps: f64) -> AmmPool {
        AmmPool { reserve_in: r_in, reserve_out: r_out, fee_bps, amp: 0.0, n_tokens: 2 }
    }

    #[test]
    fn no_arb_returns_negative() {
        // Same pool both directions → guaranteed loss
        let p = make_pool(100_000.0, 100_000.0, 30.0);
        let profit = net_profit_two_leg(&p, &p, 1000.0, 4.0, 800_000, 1_000, 10.0);
        assert!(profit < 0.0, "should be unprofitable: {profit}");
    }

    #[test]
    fn clear_arb_returns_positive() {
        // pool_a: buy ETH at 2000 (reserves: 1000 USDC per 0.5 ETH) effectively 2000
        // pool_b: sell ETH at 2100
        let pool_a = make_pool(1_000_000.0, 500.0,   0.0); // USDC→ETH at 2000
        let pool_b = make_pool(500.0, 1_050_000.0,   0.0); // ETH→USDC at 2100
        let profit = net_profit_two_leg(
            &pool_a, &pool_b,
            10_000.0,  // borrow 10k USDC
            4.0,       // 0.04% flash fee
            800_000,   // gas estimate
            1_000_000, // base fee wei
            10.0,      // HYPE price $10
        );
        assert!(profit > 0.0, "should be profitable: {profit}");
    }

    #[test]
    fn negative_borrow_is_neg_inf() {
        let p = make_pool(100_000.0, 100_000.0, 30.0);
        let profit = net_profit_two_leg(&p, &p, -1.0, 4.0, 800_000, 1_000, 10.0);
        assert_eq!(profit, f64::NEG_INFINITY);
    }
}
