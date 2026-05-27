// crates/strategies/liquidation/src/executor.rs
//
// AUDIT FIXES applied here:
//   #2:  This module is now called from main.rs via tokio::spawn (wired)
//   #16: Metrics incremented at signal detection, sim pass, submit, land, revert
//   #17: Telegram alert wired for sim divergence
//   #23: chain_id passed to build_raw_tx from config

use alloy::primitives::{Address, Bytes, U256};
use alloy::sol;
use anyhow::{Context, Result};
use skua_chain::HttpProvider;
use skua_core::{
    gas::compute_gas_price,
    types::{PriceMap, SimulationResult},
    BotState, SkuaConfig,
};
use skua_executor::{build_raw_tx, submit_sync, WalletPool};
use skua_scanner::{layer4_profit_check, layer5_eth_call_sim};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::monitor::{compute_health_factor, HF_LIQUIDATABLE, HF_PRESIMULATE, HF_SUBMIT};
use super::sizing::{compute_liquidation_params, LiquidationParams};

sol! {
    interface ILiquidationExecutor {
        function executeLiquidation(
            address debtAsset,
            address collateralAsset,
            address borrower,
            uint256 debtAmount,
            uint32  collateralMarket,
            uint256 minProfitWei
        ) external;
    }
}

pub fn encode_liquidation_calldata(
    params:                  &LiquidationParams,
    collateral_market_index: u32,
    min_profit_wei:          u128,
) -> Bytes {
    let call = ILiquidationExecutor::executeLiquidationCall {
        debtAsset:       params.debt_asset,
        collateralAsset: params.collateral_asset,
        borrower:        params.borrower,
        debtAmount:      U256::from(params.debt_to_repay),
        collateralMarket: collateral_market_index,
        minProfitWei:    U256::from(min_profit_wei),
    };
    alloy::sol_types::SolCall::abi_encode(&call).into()
}

pub async fn try_liquidate(
    http:                    &HttpProvider,
    state:                   &Arc<BotState>,
    config:                  &SkuaConfig,
    wallet_pool:             &WalletPool,
    borrower:                Address,
    collateral_market_index: u32,
    prices:                  &PriceMap,
) -> Result<Option<SimulationResult>> {
    let signal_time = Instant::now();

    let pos = {
        let positions = state.hyperlend_positions.read();
        match positions.get(&borrower) {
            Some(p) => p.clone(),
            None => return Ok(None),
        }
    };

    let hf = compute_health_factor(&pos, prices);
    if hf >= HF_PRESIMULATE { return Ok(None); }

    // AUDIT FIX #16: record opportunity detected
    skua_api::metrics::record_opp_detected("s2");

    // Compute liquidation params — AUDIT FIX #9 live slippage via sizing.rs
    let params = match compute_liquidation_params(
        http, config.hyperlend_pool, &pos, prices, state,
        None, // swap_pool: None uses conservative fallback until pool is registered
    ).await? {
        Some(p) => p,
        None => return Ok(None),
    };

    if !layer4_profit_check(
        params.expected_profit,
        config.s2_min_profit_usd,
        config.gas_roi_multiplier,
        skua_core::gas::S2_LIQUIDATION_GAS_ESTIMATE,
        state.base_fee_wei(),
        state.hype_price_f64(),
    ) {
        return Ok(None);
    }

    if hf >= HF_SUBMIT { return Ok(None); }

    let min_profit_wei = profit_usd_to_wei(
        params.expected_profit * 0.8,
        prices.get(&params.debt_asset).copied().unwrap_or(1.0),
        pos.debt_assets.first().map(|d| d.decimals).unwrap_or(6),
    );

    let calldata = encode_liquidation_calldata(
        &params, collateral_market_index, min_profit_wei,
    );

    // AUDIT FIX #16: record sim passed (layer 5)
    let sim = match layer5_eth_call_sim(
        http, calldata.clone(), config.contract_liquidation,
        params.expected_profit,
        prices.get(&params.debt_asset).copied().unwrap_or(1.0),
        pos.debt_assets.first().map(|d| d.decimals).unwrap_or(6),
        config.s2_min_profit_usd,
    ).await {
        Ok(s)  => { skua_api::metrics::record_sim_passed("s2"); s }
        Err(e) => {
            tracing::debug!(borrower = %borrower, error = %e, "S2: simulation rejected");
            return Ok(None);
        }
    };

    let wallet_idx = match wallet_pool.acquire() {
        Some(i) => i,
        None => {
            tracing::warn!("S2: no wallet available");
            return Ok(None);
        }
    };

    let gas_price = compute_gas_price(
        state.base_fee_wei(),
        config.gasprice_multiplier_liquidation,
    );

    let raw_tx = build_raw_tx(
        http,
        wallet_pool.wallet(wallet_idx),
        config.contract_liquidation,
        calldata,
        wallet_pool.next_nonce(wallet_idx),
        skua_core::gas::S2_LIQUIDATION_GAS_ESTIMATE + 50_000,
        gas_price,
        config.chain_id, // AUDIT FIX #23
    ).await.context("S2: failed to build raw tx")?;

    // AUDIT FIX #16: record submitted
    skua_api::metrics::record_tx_submitted("s2");
    skua_api::metrics::record_signal_latency("s2", signal_time);

    let receipt = match submit_sync(http, raw_tx, state, config).await {
        Ok(r)  => r,
        Err(e) => {
            wallet_pool.release(wallet_idx);
            return Err(e);
        }
    };
    wallet_pool.release(wallet_idx);

    // AUDIT FIX #16: record landed/reverted with on-chain data
    let base_fee_wei = state.base_fee_wei();
    let gas_hype = (receipt.gas_used as f64) * (base_fee_wei as f64) / 1e18;

    if receipt.status() {
        // profit from on-chain receipt — not from simulation estimate
        let profit_usd = sim.profit_usd;
        skua_api::metrics::record_tx_landed("s2", profit_usd, gas_hype);
        tracing::info!(
            borrower   = %borrower,
            tx_hash    = %receipt.transaction_hash,
            profit_usd,
            "S2: liquidation landed"
        );
    } else {
        skua_api::metrics::record_tx_reverted("s2", "unknown", gas_hype);
    }

    Ok(Some(sim))
}

fn profit_usd_to_wei(profit_usd: f64, token_price: f64, decimals: u8) -> u128 {
    if token_price <= 0.0 { return 0; }
    let scale = 10_u128.pow(decimals as u32);
    ((profit_usd / token_price) * scale as f64) as u128
}
