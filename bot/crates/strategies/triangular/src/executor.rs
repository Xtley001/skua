// crates/strategies/triangular/src/executor.rs
//
// AUDIT FIXES:
//   #16: metrics incremented at each pipeline stage
//   #23: chain_id from config in build_raw_tx

use alloy::primitives::{Address, Bytes, U256};
use alloy::sol;
use anyhow::{Context, Result};
use skua_chain::HttpProvider;
use skua_core::{
    gas::compute_gas_price,
    BotState, SkuaConfig,
};
use skua_executor::{build_raw_tx, submit_sync, WalletPool};
use skua_scanner::{layer4_profit_check, layer5_eth_call_sim};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::graph::{ArbGraph, ArbRoute};

sol! {
    struct Hop { address pool; address tokenIn; address tokenOut; }
    interface ITriArbExecutor {
        function executeTriArb(
            address     flashAsset,
            uint256     flashAmount,
            Hop[]       hops,
            uint256     minProfitWei
        ) external;
    }
}

fn encode_tri_arb_calldata(
    flash_asset:      Address,
    flash_amount_wei: u128,
    route:            &ArbRoute,
    min_profit_wei:   u128,
) -> Bytes {
    let hops: Vec<Hop> = route.hops.iter().map(|h| Hop {
        pool:     h.pool,
        tokenIn:  h.token_in,
        tokenOut: h.token_out,
    }).collect();
    let call = ITriArbExecutor::executeTriArbCall {
        flashAsset:   flash_asset,
        flashAmount:  U256::from(flash_amount_wei),
        hops,
        minProfitWei: U256::from(min_profit_wei),
    };
    alloy::sol_types::SolCall::abi_encode(&call).into()
}

pub async fn try_execute_best_route(
    http:                &HttpProvider,
    state:               &Arc<BotState>,
    config:              &SkuaConfig,
    wallet_pool:         &WalletPool,
    flash_available_usd: f64,
    token_price_usd:     f64,
) -> Result<Option<String>> {
    if !state.s3_enabled.load(Ordering::Relaxed) { return Ok(None); }

    let signal_time = Instant::now();
    let graph  = ArbGraph::build(state);
    let routes = graph.find_cycles();
    if routes.is_empty() { return Ok(None); }

    let best = routes.iter()
        .filter_map(|route| {
            graph.evaluate_route(route, state, config, flash_available_usd, token_price_usd)
                .map(|sizing| (route, sizing))
        })
        .max_by(|(_, a), (_, b)| {
            a.expected_profit_usd.partial_cmp(&b.expected_profit_usd).unwrap()
        });

    let (route, sizing) = match best {
        Some(r) => r,
        None    => return Ok(None),
    };

    // AUDIT FIX #16: record opp detected
    skua_api::metrics::record_opp_detected("s3");
    skua_api::metrics::record_sizing("s3", sizing.optimal_amount_usd);

    if !layer4_profit_check(
        sizing.expected_profit_usd,
        config.s3_min_profit_usd,
        config.gas_roi_multiplier,
        skua_core::gas::S3_TRI_ARB_3HOP_GAS_ESTIMATE,
        state.base_fee_wei(),
        state.hype_price_f64(),
    ) { return Ok(None); }

    let min_profit_wei = if token_price_usd > 0.0 {
        (sizing.expected_profit_usd * 0.8 / token_price_usd
            * 10_u128.pow(route.start_token_decimals as u32) as f64) as u128
    } else { 0 };

    let calldata = encode_tri_arb_calldata(
        route.start_token, sizing.optimal_amount_wei, route, min_profit_wei,
    );

    let sim = match layer5_eth_call_sim(
        http, calldata.clone(), config.contract_tri_arb,
        sizing.expected_profit_usd, token_price_usd,
        route.start_token_decimals, config.s3_min_profit_usd,
    ).await {
        Ok(s)  => { skua_api::metrics::record_sim_passed("s3"); s }
        Err(e) => {
            tracing::debug!(error = %e, "S3: simulation rejected");
            return Ok(None);
        }
    };

    let wallet_idx = match wallet_pool.acquire() {
        Some(i) => i,
        None => { tracing::warn!("S3: no wallet available"); return Ok(None); }
    };

    let gas_price = compute_gas_price(
        state.base_fee_wei(), config.gasprice_multiplier_arb,
    );

    let raw_tx = build_raw_tx(
        http, wallet_pool.wallet(wallet_idx),
        config.contract_tri_arb, calldata,
        wallet_pool.next_nonce(wallet_idx),
        skua_core::gas::S3_TRI_ARB_3HOP_GAS_ESTIMATE + 50_000,
        gas_price,
        config.chain_id, // AUDIT FIX #23
    ).await.context("S3: build_raw_tx failed")?;

    skua_api::metrics::record_tx_submitted("s3");
    skua_api::metrics::record_signal_latency("s3", signal_time);

    let receipt = match submit_sync(http, raw_tx, state, config).await {
        Ok(r)  => r,
        Err(e) => { wallet_pool.release(wallet_idx); return Err(e); }
    };
    wallet_pool.release(wallet_idx);

    let gas_hype = (receipt.gas_used as f64) * (state.base_fee_wei() as f64) / 1e18;
    if receipt.status() {
        skua_api::metrics::record_tx_landed("s3", sim.profit_usd, gas_hype);
        tracing::info!(
            tx_hash    = %receipt.transaction_hash,
            profit_usd = sim.profit_usd,
            "S3: triangular arb landed"
        );
        Ok(Some(receipt.transaction_hash.to_string()))
    } else {
        skua_api::metrics::record_tx_reverted("s3", "unknown", gas_hype);
        Ok(None)
    }
}
