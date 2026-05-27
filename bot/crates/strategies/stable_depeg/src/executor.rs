// crates/strategies/stable_depeg/src/executor.rs
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
    state::PoolState,
    BotState, SkuaConfig,
};
use skua_executor::{build_raw_tx, submit_sync, WalletPool};
use skua_scanner::{check_depeg, layer4_profit_check, layer5_eth_call_sim};
use skua_simulation::{net_profit_stable_depeg, optimal_borrow_size, AmmPool};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

sol! {
    interface IStableDepegExecutor {
        function executeDepegArb(
            address flashAsset,
            uint256 flashAmount,
            address poolBuy,
            address poolSell,
            uint32  oracleIndex,
            uint256 minProfitWei
        ) external;
    }
}

#[derive(Debug, Clone)]
pub struct StableToken {
    pub token:              Address,
    pub symbol:             String,
    pub oracle_token_index: u32,
    pub decimals:           u8,
    pub pool_a:             Address,
    pub pool_b:             Address,
}

pub async fn scan_and_execute(
    http:        &HttpProvider,
    state:       &Arc<BotState>,
    config:      &SkuaConfig,
    wallet_pool: &WalletPool,
    stables:     &[StableToken],
) -> Result<()> {
    if !state.s4_enabled.load(Ordering::Relaxed) { return Ok(()); }
    for stable in stables {
        if let Err(e) = check_stable(http, state, config, wallet_pool, stable).await {
            tracing::warn!(
                token  = %stable.token,
                symbol = %stable.symbol,
                error  = %e,
                "S4: check_stable failed"
            );
        }
    }
    Ok(())
}

async fn check_stable(
    http:        &HttpProvider,
    state:       &Arc<BotState>,
    config:      &SkuaConfig,
    wallet_pool: &WalletPool,
    stable:      &StableToken,
) -> Result<()> {
    let signal_time = Instant::now();

    // Oracle price — never hardcoded as 1.0
    let oracle_raw = {
        use skua_hypercore::PrecompileReader;
        let reader = PrecompileReader::new(http.clone());
        match reader.oracle_px(stable.oracle_token_index).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(token = %stable.token, error = %e, "S4: oracle read failed");
                return Ok(());
            }
        }
    };
    let oracle_price = oracle_raw as f64 / 1e8;

    let (pool_a_state, pool_b_state) = {
        let pools = state.pool_states.read();
        match (pools.get(&stable.pool_a).cloned(), pools.get(&stable.pool_b).cloned()) {
            (Some(a), Some(b)) => (a, b),
            _ => return Ok(()),
        }
    };

    let amm_price_a = stable_pool_price(&pool_a_state, stable.token);
    let amm_price_b = stable_pool_price(&pool_b_state, stable.token);
    let (buy_pool_state, sell_pool_state, buy_pool_addr, sell_pool_addr, amm_price) =
        if (amm_price_a - oracle_price).abs() >= (amm_price_b - oracle_price).abs() {
            (&pool_a_state, &pool_b_state, stable.pool_a, stable.pool_b, amm_price_a)
        } else {
            (&pool_b_state, &pool_a_state, stable.pool_b, stable.pool_a, amm_price_b)
        };

    let signal = match check_depeg(
        amm_price, oracle_price, config.s4_depeg_threshold_bps, stable.token,
    ) {
        Some(s) => s,
        None    => return Ok(()),
    };

    tracing::info!(
        symbol    = %stable.symbol,
        amm_price, oracle_price, delta_bps = signal.delta_bps,
        "S4: depeg detected"
    );
    skua_api::metrics::record_opp_detected("s4");

    // Only handle depegged-lower case
    if !signal.depegged_lower { return Ok(()); }

    let make_amm = |ps: &PoolState, token_in: Address| -> AmmPool {
        let (r_in, r_out) = if ps.token_a == token_in {
            (ps.reserve_a as f64, ps.reserve_b as f64)
        } else {
            (ps.reserve_b as f64, ps.reserve_a as f64)
        };
        AmmPool { reserve_in: r_in, reserve_out: r_out, fee_bps: ps.fee_bps as f64,
                  amp: ps.amp, n_tokens: 2 }
    };

    let amm_buy  = make_amm(buy_pool_state, stable.token);
    let amm_sell = make_amm(sell_pool_state, stable.token);

    let flash_fee_bps = state.balancer_fee_bps.load(Ordering::Relaxed) as f64;
    let base_fee_wei  = state.base_fee_wei();
    let hype_usd      = state.hype_price_f64();

    let flash_available_usd = (buy_pool_state.reserve_a as f64)
        .min(buy_pool_state.reserve_b as f64)
        / 10_f64.powi(stable.decimals as i32)
        * oracle_price;

    let (p_buy, p_sell) = (amm_buy, amm_sell);
    let r_in_usd  = buy_pool_state.reserve_a as f64 / 10_f64.powi(stable.decimals as i32) * oracle_price;
    let r_out_usd = buy_pool_state.reserve_b as f64 / 10_f64.powi(stable.decimals as i32) * oracle_price;

    let profit_fn = move |x: f64| net_profit_stable_depeg(
        &p_buy, &p_sell, x, flash_fee_bps,
        skua_core::gas::S4_DEPEG_GAS_ESTIMATE, base_fee_wei, hype_usd,
    );

    let sizing = match optimal_borrow_size(
        profit_fn,
        flash_available_usd,
        config.hard_cap_usd,
        oracle_price,
        stable.decimals,
        skua_core::types::FlashProvider::Balancer,
        r_in_usd, r_out_usd,
        buy_pool_state.amp,
        sell_pool_state.amp,
    ) {
        Some(s) => s,
        None    => return Ok(()),
    };

    skua_api::metrics::record_sizing("s4", sizing.optimal_amount_usd);

    if !layer4_profit_check(
        sizing.expected_profit_usd, config.s4_min_profit_usd,
        config.gas_roi_multiplier,
        skua_core::gas::S4_DEPEG_GAS_ESTIMATE,
        base_fee_wei, hype_usd,
    ) { return Ok(()); }

    let min_profit_wei = if oracle_price > 0.0 {
        (sizing.expected_profit_usd * 0.8 / oracle_price
            * 10_u128.pow(stable.decimals as u32) as f64) as u128
    } else { 0 };

    let calldata = encode_depeg_calldata(
        stable.token, sizing.optimal_amount_wei,
        buy_pool_addr, sell_pool_addr,
        stable.oracle_token_index, min_profit_wei,
    );

    let sim = match layer5_eth_call_sim(
        http, calldata.clone(), config.contract_stable_depeg,
        sizing.expected_profit_usd, oracle_price, stable.decimals,
        config.s4_min_profit_usd,
    ).await {
        Ok(s)  => { skua_api::metrics::record_sim_passed("s4"); s }
        Err(e) => {
            tracing::debug!(symbol = %stable.symbol, error = %e, "S4: simulation rejected");
            return Ok(());
        }
    };

    let wallet_idx = match wallet_pool.acquire() {
        Some(i) => i,
        None => { tracing::warn!("S4: no wallet available"); return Ok(()); }
    };

    let gas_price = compute_gas_price(state.base_fee_wei(), config.gasprice_multiplier_depeg);

    let raw_tx = build_raw_tx(
        http, wallet_pool.wallet(wallet_idx),
        config.contract_stable_depeg, calldata,
        wallet_pool.next_nonce(wallet_idx),
        skua_core::gas::S4_DEPEG_GAS_ESTIMATE + 50_000,
        gas_price,
        config.chain_id, // AUDIT FIX #23
    ).await.context("S4: build_raw_tx failed")?;

    skua_api::metrics::record_tx_submitted("s4");
    skua_api::metrics::record_signal_latency("s4", signal_time);

    let receipt = match submit_sync(http, raw_tx, state, config).await {
        Ok(r)  => r,
        Err(e) => { wallet_pool.release(wallet_idx); return Err(e); }
    };
    wallet_pool.release(wallet_idx);

    let gas_hype = (receipt.gas_used as f64) * (state.base_fee_wei() as f64) / 1e18;
    if receipt.status() {
        skua_api::metrics::record_tx_landed("s4", sim.profit_usd, gas_hype);
        tracing::info!(
            symbol    = %stable.symbol,
            tx_hash   = %receipt.transaction_hash,
            profit_usd = sim.profit_usd,
            "S4: depeg arb landed"
        );
    } else {
        skua_api::metrics::record_tx_reverted("s4", "unknown", gas_hype);
    }

    Ok(())
}

fn stable_pool_price(ps: &PoolState, stable_token: Address) -> f64 {
    let (r_stable, r_other) = if ps.token_a == stable_token {
        (ps.reserve_a as f64, ps.reserve_b as f64)
    } else {
        (ps.reserve_b as f64, ps.reserve_a as f64)
    };
    if r_stable == 0.0 { return 0.0; }
    r_other / r_stable
}

fn encode_depeg_calldata(
    flash_asset:      Address,
    flash_amount_wei: u128,
    pool_buy:         Address,
    pool_sell:        Address,
    oracle_index:     u32,
    min_profit_wei:   u128,
) -> Bytes {
    let call = IStableDepegExecutor::executeDepegArbCall {
        flashAsset:   flash_asset,
        flashAmount:  U256::from(flash_amount_wei),
        poolBuy:      pool_buy,
        poolSell:     pool_sell,
        oracleIndex:  oracle_index,
        minProfitWei: U256::from(min_profit_wei),
    };
    alloy::sol_types::SolCall::abi_encode(&call).into()
}
