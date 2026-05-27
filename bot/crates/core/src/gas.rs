// crates/core/src/gas.rs
//
// Profiled gas estimates from `forge test --gas-report` on HyperEVM testnet.
// These values feed the profit function. They must never be 0.
// Update after every testnet run if measurements change.

/// S1: EVM leg (Phase 1 only — CoreWriter is off-chain, async)
pub const S1_PHASE1_GAS_ESTIMATE: u64 = 550_000;

/// S2: Full liquidation (flash borrow → liquidate → swap → repay → sweep)
pub const S2_LIQUIDATION_GAS_ESTIMATE: u64 = 850_000;

/// S3: 3-hop triangular arb (flash borrow → 3 swaps → repay → sweep)
pub const S3_TRI_ARB_3HOP_GAS_ESTIMATE: u64 = 750_000;

/// S4: Stablecoin depeg arb (flash borrow → 2 swaps → repay → sweep)
pub const S4_DEPEG_GAS_ESTIMATE: u64 = 500_000;

/// Pre-simulation call (approximate — used to estimate sim overhead for latency budgeting)
pub const ETH_CALL_GAS_OVERHEAD: u64 = 200_000;

/// Compute gasPrice for a transaction.
///
/// HyperBFT burns both base fee and priority fee — there is no validator tip market.
/// The sole ordering lever is gasPrice = base_fee × multiplier.
/// At near-zero HYPE cost, aggressive multipliers (50×–500×) are essentially free.
///
/// `base_fee` is the base fee from the latest block header (wei).
/// `multiplier` is the per-strategy multiplier read from `SkuaConfig`.
#[inline]
pub fn compute_gas_price(base_fee: u128, multiplier: u64) -> u128 {
    base_fee.saturating_mul(multiplier as u128)
}

/// Compute the USD cost of gas for a transaction.
///
/// `base_fee_wei` is in wei (10^-18 HYPE per gas unit).
/// `gas_units`    is the profiled estimate for the strategy.
/// `hype_usd`     is the live HYPE price from the HyperCore oracle precompile.
#[inline]
pub fn gas_cost_usd(base_fee_wei: u128, gas_units: u64, hype_usd: f64) -> f64 {
    let gas_hype = (base_fee_wei as f64) * (gas_units as f64) / 1e18;
    gas_hype * hype_usd
}
