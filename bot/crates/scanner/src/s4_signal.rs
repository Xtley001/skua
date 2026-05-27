// crates/scanner/src/s4_signal.rs
//
// S4 signal: detect when a stablecoin AMM price deviates from HyperCore oracle price.
// oracle_price comes from HyperCore oraclePx precompile — NEVER hardcoded as 1.0.
//
// Covered stables: USDC, USDT, feUSD at launch.
// USDH: add when contract address is published by Hyperliquid — no pre-coded address.

use alloy::primitives::Address;

/// A stablecoin depeg signal.
#[derive(Debug, Clone)]
pub struct DepegSignal {
    /// Token that has depegged
    pub token:          Address,
    /// true = AMM price < oracle price (stable trading at discount)
    pub depegged_lower: bool,
    /// Deviation in bps
    pub delta_bps:      f64,
    /// AMM price derived from pool reserves via StableSwap invariant
    pub amm_price:      f64,
    /// Oracle price from HyperCore precompile (oraclePx)
    pub oracle_price:   f64,
}

/// Check a single stable token for depeg.
///
/// # Arguments
/// * `amm_price`      – Computed from pool reserves via StableSwap invariant
/// * `oracle_price`   – From HyperCore oraclePx precompile — never hardcoded
/// * `threshold_bps`  – From SkuaConfig.s4_depeg_threshold_bps (start: 15)
/// * `token`          – The stablecoin address
///
/// Returns None if oracle_price is zero — never act on zero oracle.
pub fn check_depeg(
    amm_price:     f64,
    oracle_price:  f64,
    threshold_bps: f64,
    token:         Address,
) -> Option<DepegSignal> {
    // Production law: never act on zero oracle price
    if oracle_price <= 0.0 {
        return None;
    }

    let delta_bps = ((amm_price - oracle_price) / oracle_price).abs() * 10_000.0;

    if delta_bps > threshold_bps {
        Some(DepegSignal {
            token,
            depegged_lower: amm_price < oracle_price,
            delta_bps,
            amm_price,
            oracle_price,
        })
    } else {
        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;

    fn zero_addr() -> Address { Address::ZERO }

    #[test]
    fn fires_on_discount_above_threshold() {
        let signal = check_depeg(0.99, 1.00, 15.0, zero_addr());
        assert!(signal.is_some());
        let s = signal.unwrap();
        assert!(s.depegged_lower);
        assert!((s.delta_bps - 100.0).abs() < 1.0, "delta_bps = {}", s.delta_bps);
    }

    #[test]
    fn fires_on_premium_above_threshold() {
        let signal = check_depeg(1.01, 1.00, 15.0, zero_addr());
        assert!(signal.is_some());
        assert!(!signal.unwrap().depegged_lower);
    }

    #[test]
    fn no_signal_below_threshold() {
        // 5 bps < 15 bps threshold
        let signal = check_depeg(1.0005, 1.00, 15.0, zero_addr());
        assert!(signal.is_none());
    }

    #[test]
    fn no_signal_on_zero_oracle() {
        let signal = check_depeg(0.99, 0.0, 15.0, zero_addr());
        assert!(signal.is_none());
    }
}
