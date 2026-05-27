// crates/scanner/src/pipeline.rs
//
// 5-layer opportunity pipeline.
// AUDIT FIX #16/#17: layer5 now records divergence metric and returns
// structured result so caller can fire Telegram alert on divergence.

use anyhow::Result;
use alloy::primitives::{Address, Bytes};
use skua_core::{BotState, SkuaConfig, SizingResult, SimulationResult, FlashProvider};
use skua_simulation::{
    optimal_borrow_size,
    simulate_and_validate,
    effective_min_profit_usd,
};
use skua_chain::HttpProvider;
use std::sync::Arc;

// ── Layer 1: Price delta filter ───────────────────────────────────────────────
pub fn layer1_delta_filter(signal_delta_bps: f64, min_delta_bps: f64) -> bool {
    signal_delta_bps > min_delta_bps
}

// ── Layer 2: Liquidity gate ───────────────────────────────────────────────────
pub fn layer2_liquidity_gate(pool_reserve_usd: f64, min_pool_usd: f64) -> bool {
    pool_reserve_usd > min_pool_usd
}

// ── Layer 3: GSS sizing ───────────────────────────────────────────────────────
pub fn layer3_sizing<F>(
    profit_fn:           F,
    flash_available_usd: f64,
    hard_cap_usd:        f64,
    token_price_usd:     f64,
    token_decimals:      u8,
    flash_provider:      FlashProvider,
    reserve_in_usd:      f64,
    reserve_out_usd:     f64,
    amp_entry:           f64,
    amp_exit:            f64,
) -> Option<SizingResult>
where
    F: Fn(f64) -> f64,
{
    optimal_borrow_size(
        profit_fn,
        flash_available_usd,
        hard_cap_usd,
        token_price_usd,
        token_decimals,
        flash_provider,
        reserve_in_usd,
        reserve_out_usd,
        amp_entry,
        amp_exit,
    )
}

// ── Layer 4: Algebraic profit floor ──────────────────────────────────────────
pub fn layer4_profit_check(
    expected_profit_usd:     f64,
    strategy_min_profit_usd: f64,
    gas_roi_multiplier:      f64,
    gas_units:               u64,
    base_fee_wei:            u128,
    hype_price_usd:          f64,
) -> bool {
    let min = effective_min_profit_usd(
        strategy_min_profit_usd,
        gas_roi_multiplier,
        gas_units,
        base_fee_wei,
        hype_price_usd,
    );
    expected_profit_usd > min
}

// ── Layer 5: eth_call simulation ──────────────────────────────────────────────
/// Returns Ok(SimulationResult) on success.
/// Returns Err where the error message starts with "DIVERGENCE:" when the
/// divergence threshold is exceeded — callers should record the metric
/// and fire Telegram alert (#16/#17).
pub async fn layer5_eth_call_sim(
    provider:            &HttpProvider,
    calldata:            Bytes,
    contract:            Address,
    expected_profit_usd: f64,
    token_price_usd:     f64,
    token_decimals:      u8,
    min_profit_usd:      f64,
) -> Result<SimulationResult> {
    simulate_and_validate(
        provider,
        calldata,
        contract,
        expected_profit_usd,
        token_price_usd,
        token_decimals,
        min_profit_usd,
    ).await
}
