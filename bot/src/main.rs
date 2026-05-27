// src/main.rs — SKUA bot entry point
//
// AUDIT FIX #2: evaluate_strategies now actually calls strategy executors
//               via tokio::spawn (parallel, not sequential await).
// AUDIT FIX #21: panic = "abort" added to Cargo.toml (separate change).
// AUDIT FIX #23: chain_id read from config in submit.rs (separate change).

use anyhow::{Context, Result};
use skua_api::{serve as serve_api, telegram::TelegramAlerter, ApiState};
use skua_chain::{
    block_loop::block_subscription_loop,
    fee_refresh::fee_refresh_loop,
    pool_monitor::pool_monitor_loop,
    ws_with_reconnect, build_providers,
};
use skua_core::{BotState, BlockEvent, SkuaConfig};
use skua_executor::WalletPool;
use skua_hypercore::{PrecompileReader, refresh_hype_price, subscribe_l2_book};
use skua_strategy_stable_depeg::executor::{StableToken, scan_and_execute as s4_execute};
use skua_strategy_triangular::executor::try_execute_best_route as s3_execute;
use skua_strategy_liquidation::executor::try_liquidate as s2_execute;
use skua_strategy_evm_arb::executor::S1Executor;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .json()
        .init();

    tracing::info!("SKUA starting — loading configuration");

    let config = SkuaConfig::from_env()
        .context("Configuration load failed — check environment variables")?;

    tracing::info!(
        chain_id  = config.chain_id,
        api_port  = config.api_port,
        hard_cap  = config.hard_cap_usd,
        "Configuration loaded"
    );

    let state = BotState::new();

    state.s2_enabled.store(true,  Ordering::SeqCst);
    state.s3_enabled.store(true,  Ordering::SeqCst);
    state.s4_enabled.store(false, Ordering::SeqCst);
    state.s1_enabled.store(false, Ordering::SeqCst);

    tracing::info!("Connecting to RPC providers");
    let (ws, http) = build_providers(&config)
        .await
        .context("Failed to connect RPC providers")?;

    tracing::info!("Loading wallet keys and fetching initial nonces");
    let signers = load_signers_from_env()?;
    let nonces  = WalletPool::fetch_initial_nonces(&signers, &http)
        .await
        .context("Failed to fetch initial nonces")?;
    let wallet_pool = Arc::new(WalletPool::new(signers, nonces));

    tracing::info!("Reading initial precompile prices and flash fees");
    let precompile = PrecompileReader::new(http.clone());
    refresh_hype_price(state.clone(), &precompile, config.hype_market_index)
        .await
        .context("Failed to read initial HYPE price from precompile")?;
    fee_refresh_once(&state, &http, &config).await?;

    tracing::info!(
        hype_price  = state.hype_price_f64(),
        hl_fee_bps  = state.hyperlend_fee_bps.load(Ordering::Relaxed),
        bal_fee_bps = state.balancer_fee_bps.load(Ordering::Relaxed),
        "Initial state ready"
    );

    // Load HyperLend registered assets at startup
    let hyperlend_asset_addrs: Vec<alloy::primitives::Address> = {
        // Read from env — comma-separated list of asset addresses to monitor
        std::env::var("SKUA_HYPERLEND_ASSETS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    };

    let registered_assets = if !hyperlend_asset_addrs.is_empty() {
        skua_strategy_liquidation::monitor::load_registered_assets(
            &http, config.hyperlend_pool, &hyperlend_asset_addrs,
        ).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to load registered assets — S2 disabled");
            vec![]
        })
    } else {
        tracing::warn!("SKUA_HYPERLEND_ASSETS not set — S2 position indexer inactive");
        vec![]
    };

    skua_api::metrics::init_all();

    let alerter = Arc::new(TelegramAlerter::new(
        config.telegram_bot_token.clone(),
        config.telegram_chat_id.clone(),
    ));

    // AUDIT FIX #2: S1 executor with state machine
    let s1_executor = Arc::new(S1Executor::new(config.s1_signal_threshold_bps));

    // S4 stable tokens — populated from env at startup
    let s4_stables: Vec<StableToken> = load_stable_tokens_from_env();

    let (block_tx, mut block_rx) = mpsc::channel::<BlockEvent>(256);

    // ── Background tasks ──────────────────────────────────────────────────

    // Block subscription loop
    {
        let state2  = state.clone();
        let tx2     = block_tx.clone();
        let config2 = config.clone();
        tokio::spawn(async move {
            loop {
                let ws = ws_with_reconnect(&config2).await;
                if let Err(e) = block_subscription_loop(ws, state2.clone(), tx2.clone()).await {
                    tracing::error!(error = %e, "Block loop terminated — reconnecting");
                }
            }
        });
    }

    // Fee refresh loop
    {
        let state2  = state.clone();
        let http2   = http.clone();
        let config2 = config.clone();
        tokio::spawn(async move { fee_refresh_loop(state2, http2, config2).await; });
    }

    // HyperLend position indexer (S2)
    if !registered_assets.is_empty() {
        let state2  = state.clone();
        let http2   = http.clone();
        let config2 = config.clone();
        let assets2 = registered_assets.clone();
        let ws2     = ws_with_reconnect(&config).await;
        tokio::spawn(async move {
            loop {
                if let Err(e) = skua_strategy_liquidation::monitor::position_indexer_loop(
                    ws2, http2.clone(), state2.clone(),
                    config2.hyperlend_pool, assets2.clone(),
                ).await {
                    tracing::error!(error = %e, "Position indexer terminated");
                }
            }
        });
    }

    // HyperCore L2 book subscription (S1)
    {
        let state2 = state.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = subscribe_l2_book("HYPE", state2.clone()).await {
                    tracing::warn!(error = %e, "L2 book dropped — reconnecting");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        });
    }

    // Metrics sync loop
    {
        let state2 = state.clone();
        tokio::spawn(async move { metrics_sync_loop(state2).await; });
    }

    // API server
    {
        let api_state = ApiState { bot_state: state.clone(), api_key: config.api_key.clone() };
        let port = config.api_port;
        tokio::spawn(async move {
            if let Err(e) = serve_api(api_state, port).await {
                tracing::error!(error = %e, "API server died");
            }
        });
    }

    tracing::info!("All background tasks spawned — entering strategy evaluation loop");

    // ── Main strategy evaluation loop ─────────────────────────────────────
    while let Some(event) = block_rx.recv().await {
        if state.is_halted(config.max_consecutive_reverts) {
            tracing::warn!("Kill switch active — skipping block evaluation");
            // AUDIT FIX #17: alert Telegram when kill switch is active
            let alerter2 = alerter.clone();
            tokio::spawn(async move {
                alerter2.alert_kill_switch("kill switch active — bot halted").await;
            });
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        let block_num = match event {
            BlockEvent::NewBlock(n) => n,
            BlockEvent::PoolUpdated(_) | BlockEvent::PositionUpdated(_) => continue,
        };

        // Refresh HYPE price every block
        if let Err(e) = refresh_hype_price(
            state.clone(), &precompile, config.hype_market_index,
        ).await {
            tracing::error!(error = %e, "HYPE price precompile failure");
            let alerter2 = alerter.clone();
            tokio::spawn(async move { alerter2.alert_zero_price().await; });
            continue;
        }

        // AUDIT FIX #2: spawn each strategy independently — fully parallel
        spawn_strategies(
            block_num,
            state.clone(),
            config.clone(),
            wallet_pool.clone(),
            http.clone(),
            s1_executor.clone(),
            registered_assets.clone(),
            s4_stables.clone(),
        );
    }

    Err(anyhow::anyhow!("Block event channel closed — bot exiting"))
}

/// AUDIT FIX #2: each enabled strategy is spawned as an independent tokio task.
/// They run fully in parallel — no strategy blocks another.
fn spawn_strategies(
    block_num:        u64,
    state:            Arc<BotState>,
    config:           SkuaConfig,
    wallet_pool:      Arc<WalletPool>,
    http:             skua_chain::HttpProvider,
    s1_executor:      Arc<S1Executor>,
    registered_assets: Vec<skua_strategy_liquidation::monitor::RegisteredAsset>,
    s4_stables:       Vec<StableToken>,
) {
    // ── S2: liquidations ──────────────────────────────────────────────────
    if state.s2_enabled.load(Ordering::Relaxed) {
        let borrowers: Vec<alloy::primitives::Address> = {
            state.hyperlend_positions.read()
                .iter()
                .filter(|(_, pos)| {
                    pos.computed_hf < skua_strategy_liquidation::monitor::HF_PRESIMULATE
                })
                .map(|(addr, _)| *addr)
                .collect()
        };

        if !borrowers.is_empty() {
            let state2  = state.clone();
            let config2 = config.clone();
            let wp2     = wallet_pool.clone();
            let http2   = http.clone();
            let assets2 = registered_assets.clone();
            tokio::spawn(async move {
                for borrower in borrowers {
                    if let Err(e) = s2_execute(
                        &http2, &state2, &config2, &wp2,
                        borrower,
                        config2.usdc_token_index, // collateral market index
                        &Default::default(),       // prices: populated by precompile refresh
                    ).await {
                        tracing::warn!(
                            borrower = %borrower,
                            block    = block_num,
                            error    = %e,
                            "S2: liquidation check failed"
                        );
                    }
                }
            });
        }
    }

    // ── S3: triangular arb ────────────────────────────────────────────────
    if state.s3_enabled.load(Ordering::Relaxed) {
        let state2  = state.clone();
        let config2 = config.clone();
        let wp2     = wallet_pool.clone();
        let http2   = http.clone();
        tokio::spawn(async move {
            if let Err(e) = s3_execute(
                &http2, &state2, &config2, &wp2,
                /* flash_available_usd */ 1_000_000.0, // TODO: query live from Balancer
                /* token_price_usd    */ state2.hype_price_f64(),
            ).await {
                tracing::warn!(block = block_num, error = %e, "S3: route execution failed");
            }
        });
    }

    // ── S4: stable depeg ──────────────────────────────────────────────────
    if state.s4_enabled.load(Ordering::Relaxed) && !s4_stables.is_empty() {
        let state2  = state.clone();
        let config2 = config.clone();
        let wp2     = wallet_pool.clone();
        let http2   = http.clone();
        let stables = s4_stables.clone();
        tokio::spawn(async move {
            if let Err(e) = s4_execute(&http2, &state2, &config2, &wp2, &stables).await {
                tracing::warn!(block = block_num, error = %e, "S4: depeg scan failed");
            }
        });
    }

    // ── S1: EVM/Core arb ─────────────────────────────────────────────────
    if state.s1_enabled.load(Ordering::Relaxed) {
        let state2      = state.clone();
        let config2     = config.clone();
        let wp2         = wallet_pool.clone();
        let http2       = http.clone();
        let s1          = s1_executor.clone();
        tokio::spawn(async move {
            if let Err(e) = s1.tick(
                &http2, &state2, &config2, &wp2,
                alloy::primitives::Address::ZERO, // evm_pool: set per-market at deployment
                alloy::primitives::Address::ZERO, // bought_token
                alloy::primitives::Address::ZERO, // flash_asset
                config2.hype_market_index,
                state2.hype_price_f64(),
                1_000_000.0, // flash_available_usd: TODO live query
            ).await {
                tracing::warn!(block = block_num, error = %e, "S1: tick failed");
            }
        });
    }
}

fn load_signers_from_env() -> Result<Vec<alloy::signers::local::PrivateKeySigner>> {
    let mut signers = Vec::new();
    for i in 1..=10 {
        let key_var = format!("SKUA_BOT_KEY_{i}");
        match std::env::var(&key_var) {
            Ok(hex_key) => {
                // AUDIT: key never logged — map_err message contains no key value
                let signer: alloy::signers::local::PrivateKeySigner = hex_key
                    .trim_start_matches("0x")
                    .parse()
                    .map_err(|_| anyhow::anyhow!(
                        "{key_var} is not a valid private key \
                         (check format, missing 0x prefix, or wrong length — value not logged)"
                    ))?;
                signers.push(signer);
            }
            Err(_) => {
                if i <= 5 {
                    return Err(anyhow::anyhow!(
                        "Missing required env var: {key_var} (minimum 5 wallets required)"
                    ));
                }
                break;
            }
        }
    }
    tracing::info!(count = signers.len(), "Wallets loaded");
    Ok(signers)
}

async fn fee_refresh_once(
    state:  &Arc<BotState>,
    http:   &skua_chain::HttpProvider,
    config: &SkuaConfig,
) -> Result<()> {
    use alloy::sol;
    sol! {
        interface IHyperLendPool { function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128); }
        interface IBalancerVault {
            function getProtocolFeePercentages()
                external view returns (uint256, uint256 flashLoanFeePercentage, uint256);
        }
    }
    let hl_fee = IHyperLendPool::new(config.hyperlend_pool, http)
        .FLASHLOAN_PREMIUM_TOTAL().call().await
        .context("Startup: failed to read HyperLend fee")?._0 as u64;
    state.hyperlend_fee_bps.store(hl_fee, Ordering::SeqCst);

    let bal_result = IBalancerVault::new(config.balancer_vault, http)
        .getProtocolFeePercentages().call().await
        .context("Startup: failed to read Balancer fee")?;
    let bal_wad: u128 = bal_result.flashLoanFeePercentage
        .try_into().context("Balancer fee overflow")?;
    let bal_bps = (bal_wad * 10_000 / 1_000_000_000_000_000_000u128) as u64;
    state.balancer_fee_bps.store(bal_bps, Ordering::SeqCst);
    Ok(())
}

/// Load S4 stable token definitions from environment variables.
/// Format: SKUA_S4_STABLE_1=USDC:0xabc...:0:6:0xpool_a:0xpool_b
fn load_stable_tokens_from_env() -> Vec<StableToken> {
    let mut stables = Vec::new();
    for i in 1..=8 {
        let var = format!("SKUA_S4_STABLE_{i}");
        let val = match std::env::var(&var) {
            Ok(v) => v,
            Err(_) => break,
        };
        let parts: Vec<&str> = val.split(':').collect();
        if parts.len() != 6 {
            tracing::warn!(var, "Invalid S4 stable format — expected SYMBOL:ADDR:ORACLE_IDX:DECIMALS:POOL_A:POOL_B");
            continue;
        }
        let token:    alloy::primitives::Address = match parts[1].parse() { Ok(a) => a, Err(_) => continue };
        let oracle:   u32                        = match parts[2].parse() { Ok(v) => v, Err(_) => continue };
        let decimals: u8                         = match parts[3].parse() { Ok(v) => v, Err(_) => continue };
        let pool_a:   alloy::primitives::Address = match parts[4].parse() { Ok(a) => a, Err(_) => continue };
        let pool_b:   alloy::primitives::Address = match parts[5].parse() { Ok(a) => a, Err(_) => continue };
        stables.push(StableToken {
            token,
            symbol: parts[0].to_string(),
            oracle_token_index: oracle,
            decimals,
            pool_a,
            pool_b,
        });
    }
    if stables.is_empty() {
        tracing::warn!("No S4 stable tokens configured (SKUA_S4_STABLE_1..8 not set)");
    }
    stables
}

async fn metrics_sync_loop(state: Arc<BotState>) {
    use skua_api::metrics::*;
    use std::time::Duration;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        BLOCK_NUMBER.set(state.current_block.load(Ordering::Relaxed) as f64);
        BASE_FEE_GWEI.set(state.current_base_fee.load(Ordering::Relaxed) as f64 / 1e9);
        HYPE_PRICE_USD.set(state.hype_price_f64());
        BALANCER_FEE_BPS.set(state.balancer_fee_bps.load(Ordering::Relaxed) as f64);
        HYPERLEND_FEE_BPS.set(state.hyperlend_fee_bps.load(Ordering::Relaxed) as f64);
        KILL_SWITCH_ACTIVE.set(if state.kill_switch.load(Ordering::Relaxed) { 1.0 } else { 0.0 });
        CONSECUTIVE_REVERTS.set(state.consecutive_reverts.load(Ordering::Relaxed) as f64);
        GAS_SPENT_WINDOW_HYPE.set(state.gas_spent_window.load(Ordering::Relaxed) as f64 / 1e8);
    }
}
