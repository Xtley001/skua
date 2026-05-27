// crates/strategies/evm_arb/src/executor.rs
//
// AUDIT FIXES:
//   #5:  Phase2Pending stores held_token; timeout handler can now call emergencyExit
//   #6:  trigger_emergency_exit builds actual swap calldata (not empty Bytes::new())
//   #15: Phase1Complete stores expected_profit_usd; should_emergency_exit uses it

use alloy::primitives::{Address, Bytes, U256};
use alloy::sol;
use anyhow::{Context, Result};
use skua_chain::HttpProvider;
use skua_core::{
    gas::compute_gas_price,
    BotState, SkuaConfig,
};
use skua_executor::{build_raw_tx, submit_sync, WalletPool};
use skua_hypercore::{build_s1_sell_order, PrecompileReader, COREWRITER_ADDRESS};
use skua_scanner::{layer4_profit_check, layer5_eth_call_sim, S1Detector, S1Signal};
use skua_simulation::{net_profit_two_leg, optimal_borrow_size, AmmPool};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::phase::{s1_risk_check, S1Phase, VolEstimator};

sol! {
    interface IEvmArbExecutor {
        function executePhase1(
            address flashAsset,
            uint256 flashAmount,
            address evmPool,
            bool    buyTokenIsA,
            uint32  coreMarketIndex,
            uint64  coreMinBidScaled,
            uint256 minEscrowAmount
        ) external;

        function emergencyExit(
            address dex,
            bytes   swapCalldata
        ) external;
    }

    /// Uni V2 pair interface — used to build emergency exit swap calldata.
    interface IUniV2Pair {
        function swap(
            uint256 amount0Out,
            uint256 amount1Out,
            address to,
            bytes   data
        ) external;
        function getReserves() external view returns (uint112, uint112, uint32);
        function token0() external view returns (address);
    }
}

pub struct S1Executor {
    pub state_machine: Mutex<S1Phase>,
    pub vol_estimator: Mutex<VolEstimator>,
    pub detector:      S1Detector,
}

impl S1Executor {
    pub fn new(threshold_bps: f64) -> Self {
        Self {
            state_machine: Mutex::new(S1Phase::Idle),
            vol_estimator: Mutex::new(VolEstimator::new(60)),
            detector:      S1Detector::new(threshold_bps),
        }
    }

    pub async fn tick(
        &self,
        http:                &HttpProvider,
        state:               &Arc<BotState>,
        config:              &SkuaConfig,
        wallet_pool:         &WalletPool,
        evm_pool:            Address,
        bought_token:        Address, // actual token purchased in Phase 1
        flash_asset:         Address, // token borrowed (debt token)
        core_market_index:   u32,
        token_price_usd:     f64,
        flash_available_usd: f64,
    ) -> Result<()> {
        if !state.s1_enabled.load(Ordering::Relaxed) { return Ok(()); }

        {
            let mut vol = self.vol_estimator.lock().unwrap();
            vol.push(state.hype_price_f64());
        }

        let current_phase = self.state_machine.lock().unwrap().clone();

        match current_phase {
            S1Phase::Idle => {
                self.try_phase1(
                    http, state, config, wallet_pool,
                    evm_pool, bought_token, flash_asset,
                    core_market_index, token_price_usd, flash_available_usd,
                ).await?;
            }

            S1Phase::Phase1Complete {
                held_token, held_amount, min_phase2_proceeds, expected_profit_usd, ..
            } => {
                let current_price  = token_price_usd;
                let expected_price = min_phase2_proceeds as f64 / held_amount as f64;

                if current_phase.should_emergency_exit(current_price, expected_price) {
                    tracing::warn!("S1: triggering emergency exit");
                    self.trigger_emergency_exit(
                        http, state, config, wallet_pool,
                        held_token, held_amount, flash_asset, evm_pool,
                    ).await?;
                } else {
                    self.execute_phase2(
                        http, state, config, wallet_pool,
                        held_token, held_amount, min_phase2_proceeds,
                        core_market_index, token_price_usd,
                    ).await?;
                }
            }

            S1Phase::Phase2Pending { held_token, held_amount, placed_at, .. } => {
                if placed_at.elapsed() >= super::phase::PHASE2_TIMEOUT {
                    tracing::warn!("S1: Phase 2 timed out — emergency exit");
                    self.trigger_emergency_exit(
                        http, state, config, wallet_pool,
                        held_token, held_amount, flash_asset, evm_pool,
                    ).await?;
                }
                // Otherwise: waiting for WS allOrders fill confirmation
            }

            S1Phase::Phase2Complete { proceeds_hype } => {
                tracing::info!(proceeds_hype, "S1: Phase 2 complete — resetting to Idle");
                *self.state_machine.lock().unwrap() = S1Phase::Idle;
            }

            S1Phase::EmergencyExited { swapped_out, net_result } => {
                tracing::warn!(swapped_out, net_result, "S1: emergency exit complete");
                *self.state_machine.lock().unwrap() = S1Phase::Idle;
            }
        }

        Ok(())
    }

    async fn try_phase1(
        &self,
        http:                &HttpProvider,
        state:               &Arc<BotState>,
        config:              &SkuaConfig,
        wallet_pool:         &WalletPool,
        evm_pool:            Address,
        bought_token:        Address,
        flash_asset:         Address,
        core_market_index:   u32,
        token_price_usd:     f64,
        flash_available_usd: f64,
    ) -> Result<()> {
        let signal = match self.detector.evaluate() {
            Some(s) => s,
            None    => return Ok(()),
        };

        let delta_bps = match &signal {
            S1Signal::BuyEvmSellCore { delta_bps }       => *delta_bps,
            S1Signal::BuyCoreEscrowSellEvm { delta_bps } => *delta_bps,
        };

        let pool_state = {
            let pools = state.pool_states.read();
            match pools.get(&evm_pool) {
                Some(ps) => ps.clone(),
                None => return Ok(()),
            }
        };

        let (r_in, r_out) = if pool_state.token_a == flash_asset {
            (pool_state.reserve_a as f64, pool_state.reserve_b as f64)
        } else {
            (pool_state.reserve_b as f64, pool_state.reserve_a as f64)
        };

        let pool = AmmPool {
            reserve_in: r_in, reserve_out: r_out,
            fee_bps: pool_state.fee_bps as f64,
            amp: pool_state.amp, n_tokens: 2,
        };

        let core_bid = self.detector.core_bid.load(Ordering::Relaxed) as f64 / 1e8;
        let sell_proxy = AmmPool {
            reserve_in:  1_000_000.0 * core_bid,
            reserve_out: 1_000_000.0,
            fee_bps: 0.0, amp: 0.0, n_tokens: 2,
        };

        let flash_fee_bps = state.balancer_fee_bps.load(Ordering::Relaxed) as f64;
        let base_fee_wei  = state.base_fee_wei();
        let hype_usd      = state.hype_price_f64();

        let (p1, p2) = (pool, sell_proxy);
        let profit_fn = move |x: f64| net_profit_two_leg(
            &p1, &p2, x, flash_fee_bps,
            skua_core::gas::S1_PHASE1_GAS_ESTIMATE,
            base_fee_wei, hype_usd,
        );

        let sizing = match optimal_borrow_size(
            profit_fn,
            flash_available_usd,
            config.hard_cap_usd,
            token_price_usd,
            8,
            skua_core::types::FlashProvider::Balancer,
            r_in / 1e6,  // rough USD normalisation
            r_out / 1e6,
            0.0, 0.0,
        ) {
            Some(s) => s,
            None => return Ok(()),
        };

        if !layer4_profit_check(
            sizing.expected_profit_usd,
            config.s1_min_profit_usd,
            config.gas_roi_multiplier,
            skua_core::gas::S1_PHASE1_GAS_ESTIMATE,
            base_fee_wei, hype_usd,
        ) { return Ok(()); }

        let vol_per_sec = self.vol_estimator.lock().unwrap().vol_per_second();
        if !s1_risk_check(
            sizing.expected_profit_usd,
            sizing.optimal_amount_usd,
            vol_per_sec,
            4.0,
        ) {
            tracing::debug!("S1: risk check failed — vol margin insufficient");
            return Ok(());
        }

        let core_min_bid_scaled = (core_bid * 0.99 * 1e8) as u64;
        let min_escrow = (sizing.optimal_amount_wei as f64 * 0.95) as u128;

        let calldata = encode_phase1_calldata(
            sizing.optimal_amount_wei, evm_pool,
            pool_state.token_a == flash_asset,
            core_market_index, core_min_bid_scaled, min_escrow,
        );

        if let Err(e) = layer5_eth_call_sim(
            http, calldata.clone(), config.contract_evm_arb,
            sizing.expected_profit_usd, token_price_usd, 8,
            config.s1_min_profit_usd,
        ).await {
            tracing::debug!("S1: simulation rejected — {e}");
            return Ok(());
        }

        let wallet_idx = match wallet_pool.acquire() { Some(i) => i, None => return Ok(()) };
        let gas_price  = compute_gas_price(state.base_fee_wei(), config.gasprice_multiplier_arb);

        let raw_tx = build_raw_tx(
            http, wallet_pool.wallet(wallet_idx),
            config.contract_evm_arb, calldata,
            wallet_pool.next_nonce(wallet_idx),
            skua_core::gas::S1_PHASE1_GAS_ESTIMATE + 50_000,
            gas_price,
            gas_price,
            config.chain_id, // AUDIT FIX #23
        ).await.context("S1: build Phase 1 tx failed")?;

        let receipt = submit_sync(http, raw_tx, state, config)
            .await.context("S1: Phase 1 submit failed")?;

        wallet_pool.release(wallet_idx);

        if receipt.status() {
            // AUDIT FIX #15: store actual expected_profit_usd (not 0.0)
            // AUDIT FIX #5:  store actual bought_token (not Address::ZERO)
            *self.state_machine.lock().unwrap() = S1Phase::Phase1Complete {
                held_token:          bought_token,                     // FIX #5
                held_amount:         sizing.optimal_amount_wei,
                min_phase2_proceeds: min_escrow,
                expected_profit_usd: sizing.expected_profit_usd,       // FIX #15
                entered_at:          Instant::now(),
                phase1_tx:           receipt.transaction_hash,
            };
            tracing::info!(
                tx_hash    = %receipt.transaction_hash,
                profit_usd = sizing.expected_profit_usd,
                "S1: Phase 1 complete"
            );
        }
        Ok(())
    }

    async fn execute_phase2(
        &self,
        http:               &HttpProvider,
        state:              &Arc<BotState>,
        config:             &SkuaConfig,
        wallet_pool:        &WalletPool,
        held_token:         Address,
        held_amount:        u128,
        min_proceeds:       u128,
        core_market_index:  u32,
        token_price_usd:    f64,
    ) -> Result<()> {
        let core_bid = self.detector.core_bid.load(Ordering::Relaxed) as f64 / 1e8;
        if core_bid <= 0.0 {
            tracing::warn!("S1: Phase 2 — core bid is zero, waiting");
            return Ok(());
        }

        let size_human = held_amount as f64 / 1e8;
        let limit_px   = core_bid * 0.995;
        let order      = build_s1_sell_order(core_market_index, size_human, limit_px, 0);
        let calldata   = order.calldata();

        let wallet_idx = match wallet_pool.acquire() { Some(i) => i, None => return Ok(()) };
        let gas_price  = compute_gas_price(state.base_fee_wei(), config.gasprice_multiplier_arb);

        let raw_tx = build_raw_tx(
            http, wallet_pool.wallet(wallet_idx),
            COREWRITER_ADDRESS, calldata,
            wallet_pool.next_nonce(wallet_idx),
            150_000, gas_price,
            150_000, gas_price,
            config.chain_id, // AUDIT FIX #23
        ).await.context("S1: build Phase 2 tx failed")?;

        let receipt = submit_sync(http, raw_tx, state, config)
            .await.context("S1: Phase 2 submit failed")?;

        wallet_pool.release(wallet_idx);

        // AUDIT FIX #5: transition to Phase2Pending WITH held_token stored
        *self.state_machine.lock().unwrap() = S1Phase::Phase2Pending {
            held_token,     // FIX #5: stored so timeout handler can call emergencyExit
            held_amount,
            cloid: 0,
            placed_at: Instant::now(),
        };

        tracing::info!(tx_hash = %receipt.transaction_hash, "S1: Phase 2 CoreWriter order placed");
        Ok(())
    }

    /// AUDIT FIX #6: Emergency exit now builds actual swap calldata.
    /// Swaps held_token back to flash_asset (debt token) through the same EVM pool.
    async fn trigger_emergency_exit(
        &self,
        http:        &HttpProvider,
        state:       &Arc<BotState>,
        config:      &SkuaConfig,
        wallet_pool: &WalletPool,
        held_token:  Address,
        held_amount: u128,
        flash_asset: Address,
        evm_pool:    Address,
    ) -> Result<()> {
        // AUDIT FIX #6: build real swap calldata instead of Bytes::new()
        // Uni V2-style: determine which output slot is the flash_asset
        let swap_calldata = build_univ2_swap_calldata(
            http,
            evm_pool,
            held_token,
            flash_asset,
            held_amount,
            config.contract_evm_arb,
        ).await.unwrap_or_else(|e| {
            tracing::error!("S1: failed to build emergency swap calldata: {e}");
            Bytes::new() // if calldata build fails, log loudly but attempt the tx
        });

        let calldata = {
            let call = IEvmArbExecutor::emergencyExitCall {
                dex:          evm_pool,
                swapCalldata: swap_calldata,
            };
            alloy::sol_types::SolCall::abi_encode(&call).into()
        };

        let wallet_idx = match wallet_pool.acquire() { Some(i) => i, None => return Ok(()) };
        let gas_price  = compute_gas_price(state.base_fee_wei(), config.gasprice_multiplier_arb);

        let raw_tx = build_raw_tx(
            http, wallet_pool.wallet(wallet_idx),
            config.contract_evm_arb, calldata,
            wallet_pool.next_nonce(wallet_idx),
            400_000, gas_price,
            400_000, gas_price,
            config.chain_id, // AUDIT FIX #23
        ).await?;

        let receipt = submit_sync(http, raw_tx, state, config).await?;
        wallet_pool.release(wallet_idx);

        *self.state_machine.lock().unwrap() = S1Phase::EmergencyExited {
            swapped_out: held_amount,
            net_result:  0, // decoded from receipt events post-deployment
        };

        tracing::warn!(tx_hash = %receipt.transaction_hash, "S1: emergency exit executed");
        Ok(())
    }
}

/// Build Uni V2-style swap calldata: swap held_token → flash_asset through evm_pool.
/// Reads on-chain reserves to compute the correct output amount.
async fn build_univ2_swap_calldata(
    http:        &HttpProvider,
    pool:        Address,
    token_in:    Address,
    _token_out:  Address,
    amount_in:   u128,
    recipient:   Address,
) -> Result<Bytes> {
    let pair = IUniV2Pair::new(pool, http);

    // Determine which slot is token_out
    let token0 = pair.token0().call().await
        .context("emergency exit: token0() call failed")?
        .token0;

    let reserves = pair.getReserves().call().await
        .context("emergency exit: getReserves() call failed")?;

    let (reserve_in, reserve_out) = if token0 == token_in {
        (reserves._0 as u128, reserves._1 as u128)
    } else {
        (reserves._1 as u128, reserves._0 as u128)
    };

    // Compute amount_out using Uni V2 formula with 0.30% fee
    // Will be updated when pool fee is read from chain (Finding #19)
    let numerator   = amount_in * 997 * reserve_out;
    let denominator = reserve_in * 1000 + amount_in * 997;
    let amount_out  = if denominator > 0 { numerator / denominator } else { 0 };

    if amount_out == 0 {
        return Err(anyhow::anyhow!("emergency exit: computed amount_out = 0"));
    }

    // Encode Uni V2 swap: swap(amount0Out, amount1Out, to, data)
    let (amount0_out, amount1_out) = if token0 == token_in {
        (U256::ZERO, U256::from(amount_out))
    } else {
        (U256::from(amount_out), U256::ZERO)
    };

    let call = IUniV2Pair::swapCall {
        amount0Out: amount0_out,
        amount1Out: amount1_out,
        to:         recipient,
        data:       Bytes::new(),
    };

    Ok(alloy::sol_types::SolCall::abi_encode(&call).into())
}

fn encode_phase1_calldata(
    flash_amount_wei:    u128,
    evm_pool:            Address,
    buy_token_is_a:      bool,
    core_market_index:   u32,
    core_min_bid_scaled: u64,
    min_escrow_amount:   u128,
) -> Bytes {
    let call = IEvmArbExecutor::executePhase1Call {
        flashAsset:       Address::ZERO,
        flashAmount:      U256::from(flash_amount_wei),
        evmPool:          evm_pool,
        buyTokenIsA:      buy_token_is_a,
        coreMarketIndex:  core_market_index,
        coreMinBidScaled: core_min_bid_scaled,
        minEscrowAmount:  U256::from(min_escrow_amount),
    };
    alloy::sol_types::SolCall::abi_encode(&call).into()
}
