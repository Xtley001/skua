// crates/strategies/liquidation/src/sizing.rs
//
// AUDIT FIXES:
//   #7:  Close factor: dynamic based on HF (not hardcoded 50%)
//   #8:  Price lookup propagates Err instead of unwrap_or(0.0)
//   #9:  Slippage computed from live AMM math, not hardcoded 0.003

use alloy::primitives::Address;
use anyhow::{anyhow, Context, Result};
use skua_chain::HttpProvider;
use skua_core::{
    gas::{gas_cost_usd, S2_LIQUIDATION_GAS_ESTIMATE},
    state::BorrowerPosition,
    types::PriceMap,
    BotState,
};
use skua_simulation::AmmPool;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use alloy::sol;

sol! {
    interface IHyperLendPool {
        function getReserveConfigurationData(address asset)
            external view
            returns (
                uint256 decimals,
                uint256 ltv,
                uint256 liquidationThreshold,
                uint256 liquidationBonus,
                uint256 reserveFactor,
                bool usageAsCollateralEnabled,
                bool borrowingEnabled,
                bool stableBorrowRateEnabled,
                bool isActive,
                bool isFrozen
            );
    }
}

#[derive(Debug, Clone)]
pub struct LiquidationParams {
    pub borrower:          Address,
    pub debt_asset:        Address,
    pub collateral_asset:  Address,
    pub debt_to_repay:     u128,
    pub collateral_bonus:  f64,
    pub expected_profit:   f64,
    pub use_hyperlend_flash: bool,
}

/// AUDIT FIX #7: Dynamic close factor based on health factor.
/// HyperLend (Aave V3 fork): if HF < 0.95, liquidators can close 100% of debt.
/// Otherwise, close factor is 50%.
fn dynamic_close_factor_bps(health_factor: f64) -> u128 {
    if health_factor < 0.95 {
        10_000 // 100% — full liquidation allowed
    } else {
        5_000  // 50%  — standard close factor
    }
}

pub async fn compute_liquidation_params(
    http:           &HttpProvider,
    hyperlend_pool: Address,
    pos:            &BorrowerPosition,
    prices:         &PriceMap,
    state:          &Arc<BotState>,
    swap_pool:      Option<(Address, f64, f64, f64)>, // (pool_addr, r_in, r_out, fee_bps)
) -> Result<Option<LiquidationParams>> {
    if pos.debt_assets.is_empty() || pos.collateral_assets.is_empty() {
        return Ok(None);
    }

    let debt_asset       = pos.debt_assets[0].token;
    let collateral_asset = pos.collateral_assets[0].token;

    // ── AUDIT FIX #7: Dynamic close factor ──────────────────────────────
    let close_factor_bps = dynamic_close_factor_bps(pos.computed_hf);

    let total_debt_units: u128 = pos.debt_assets.iter().map(|d| d.amount).sum();
    let max_liquidatable = total_debt_units * close_factor_bps / 10_000;

    if max_liquidatable == 0 { return Ok(None); }

    // ── Read reserve config for liquidation bonus ────────────────────────
    let pool = IHyperLendPool::new(hyperlend_pool, http);
    let reserve_cfg = pool
        .getReserveConfigurationData(debt_asset)
        .call()
        .await
        .context("Failed to read reserve config")?;

    let liq_bonus = reserve_cfg.liquidationBonus as f64 / 10_000.0;

    // ── AUDIT FIX #8: Propagate price errors instead of unwrap_or(0.0) ──
    let debt_price = prices
        .get(&debt_asset)
        .copied()
        .ok_or_else(|| anyhow!("No price for debt asset {} — price feed gap", debt_asset))?;

    let collateral_price = prices
        .get(&collateral_asset)
        .copied()
        .ok_or_else(|| anyhow!("No price for collateral asset {} — price feed gap", collateral_asset))?;

    if debt_price <= 0.0 {
        return Err(anyhow!("Zero debt price for {} — precompile returned invalid data", debt_asset));
    }
    if collateral_price <= 0.0 {
        return Err(anyhow!("Zero collateral price for {} — precompile returned invalid data", collateral_asset));
    }

    let debt_decimals = pos.debt_assets[0].decimals;
    let debt_usd = (max_liquidatable as f64 / 10_f64.powi(debt_decimals as i32)) * debt_price;

    let hl_fee_bps  = state.hyperlend_fee_bps.load(Ordering::Relaxed) as f64;
    let flash_fee   = debt_usd * hl_fee_bps / 10_000.0;

    let collateral_usd_received = debt_usd * liq_bonus;

    // ── AUDIT FIX #9: Live slippage from AMM math ────────────────────────
    let swap_slippage_usd = if let Some((_pool_addr, r_in, r_out, fee_bps)) = swap_pool {
        // collateral→debt swap: use AmmPool to model actual slippage
        let amm = AmmPool {
            reserve_in:  r_in,
            reserve_out: r_out,
            fee_bps,
            amp:     0.0,
            n_tokens: 2,
        };
        // Amount of collateral we receive (in token units, not USD)
        let collateral_decimals = pos.collateral_assets[0].decimals;
        let collateral_amount = collateral_usd_received / collateral_price
            * 10_f64.powi(collateral_decimals as i32);

        // Model the swap output
        let debt_received = amm.get_output(collateral_amount);
        let debt_received_usd = debt_received
            / 10_f64.powi(debt_decimals as i32)
            * debt_price;

        // Slippage = expected USD - actual USD received
        (collateral_usd_received - debt_received_usd).max(0.0)
    } else {
        // No pool data available — use conservative 0.5% estimate but warn
        tracing::warn!(
            borrower = %pos.address,
            "No swap pool data for slippage estimate — using 0.5% fallback"
        );
        collateral_usd_received * 0.005
    };

    let base_fee_wei = state.base_fee_wei();
    let hype_usd     = state.hype_price_f64();
    let gas_usd      = gas_cost_usd(base_fee_wei, S2_LIQUIDATION_GAS_ESTIMATE, hype_usd);

    let expected_profit = collateral_usd_received
        - debt_usd
        - flash_fee
        - swap_slippage_usd
        - gas_usd;

    tracing::debug!(
        borrower              = %pos.address,
        hf                    = pos.computed_hf,
        close_factor_bps,
        debt_usd,
        collateral_usd_received,
        flash_fee,
        swap_slippage_usd,
        gas_usd,
        expected_profit,
        "Liquidation sizing"
    );

    Ok(Some(LiquidationParams {
        borrower:         pos.address,
        debt_asset,
        collateral_asset,
        debt_to_repay:   max_liquidatable,
        collateral_bonus: liq_bonus,
        expected_profit,
        use_hyperlend_flash: true,
    }))
}
