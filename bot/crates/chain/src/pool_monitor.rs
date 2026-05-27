// crates/chain/src/pool_monitor.rs
//
// Primary: subscribe to Sync / Swap / PoolBalanceChanged log events.
// Reconciliation: multicall every 10 blocks to correct event drift.
// Never use HTTP polling as primary.

use anyhow::{Context, Result};
use alloy::primitives::{Address, B256};
use alloy::sol;
use alloy::providers::Provider;
use alloy::rpc::types::{Filter, Log};
use skua_core::{BotState, BlockEvent, state::PoolState, state::PoolType};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use super::providers::{WsProvider, HttpProvider};

// ── Solidity ABI fragments ────────────────────────────────────────────────────

sol! {
    /// Uniswap V2 / constant-product Sync event
    event Sync(uint112 reserve0, uint112 reserve1);

    /// Balancer V3 PoolBalanceChanged event (simplified)
    event PoolBalanceChanged(
        bytes32 indexed poolId,
        address indexed liquidityProvider,
        address[] tokens,
        int256[] deltas,
        uint256[] protocolFeeAmounts
    );

    interface IUniV2Pair {
        function getReserves()
            external view
            returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee()    external view returns (uint24);  // some forks expose this
    }
}

/// Known event topic hashes (keccak256 of signature)
const SYNC_TOPIC: &str =
    "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1";

/// Register a set of pool addresses to monitor and start the log subscription loop.
/// `pool_addrs` is populated from config / discovery at startup.
pub async fn pool_monitor_loop(
    ws:           WsProvider,
    http:         HttpProvider,
    state:        Arc<BotState>,
    pool_addrs:   Vec<Address>,
    event_tx:     mpsc::Sender<BlockEvent>,
) -> Result<()> {
    // Build log filter for all known pools
    let filter = Filter::new()
        .address(pool_addrs.clone())
        .event_signature(SYNC_TOPIC.parse::<B256>().unwrap());

    let mut log_stream = ws
        .subscribe_logs(&filter)
        .await
        .context("Failed to subscribe to pool Sync logs")?;

    tracing::info!(pools = pool_addrs.len(), "Pool monitor subscribed");

    let mut last_reconcile_block: u64 = 0;

    loop {
        // ── Reconcile every 10 blocks (catches dropped events) ──────────
        let current_block = state.current_block.load(Ordering::Relaxed);
        if current_block >= last_reconcile_block + 10 {
            if let Err(e) = reconcile_pools(&http, &state, &pool_addrs).await {
                tracing::warn!(error = %e, "Pool reconciliation failed");
            }
            last_reconcile_block = current_block;
        }

        // ── Process next log event ───────────────────────────────────────
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            log_stream.next(),
        )
        .await
        {
            Ok(Some(log)) => {
                if let Err(e) = handle_sync_log(log, &state, &event_tx).await {
                    tracing::warn!(error = %e, "Failed to handle Sync log");
                }
            }
            Ok(None) => {
                return Err(anyhow::anyhow!("Pool log subscription stream terminated"));
            }
            Err(_timeout) => {
                // No log in 5s is normal — just loop
            }
        }
    }
}

/// Decode a Sync log and update the corresponding PoolState.
async fn handle_sync_log(
    log:      Log,
    state:    &Arc<BotState>,
    event_tx: &mpsc::Sender<BlockEvent>,
) -> Result<()> {
    let pool_addr = log.address();
    let block_num = log.block_number.unwrap_or(0);

    // Decode Sync(reserve0, reserve1)
    let decoded = Sync::decode_log(&log.inner, true)
        .context("Failed to decode Sync log")?;

    let r0 = decoded.reserve0 as u128;
    let r1 = decoded.reserve1 as u128;

    {
        let mut pools = state.pool_states.write();
        if let Some(ps) = pools.get_mut(&pool_addr) {
            ps.reserve_a    = r0;
            ps.reserve_b    = r1;
            ps.last_updated = block_num;
        }
        // Unknown pool — skip (it's registered at startup)
    }

    tracing::debug!(
        pool    = %pool_addr,
        block   = block_num,
        r0, r1,
        "Pool Sync event"
    );

    let _ = event_tx.try_send(BlockEvent::PoolUpdated(pool_addr));
    Ok(())
}

/// Multicall reconciliation: re-read reserves for every known pool.
/// Corrects any state drift from dropped or misordered events.
async fn reconcile_pools(
    http:       &HttpProvider,
    state:      &Arc<BotState>,
    pool_addrs: &[Address],
) -> Result<()> {
    // For each pool, call getReserves() individually.
    // TODO: batch via multicall3 once deployed on HyperEVM testnet.
    for &addr in pool_addrs {
        let pair = IUniV2Pair::new(addr, http);
        match pair.getReserves().call().await {
            Ok(res) => {
                let mut pools = state.pool_states.write();
                if let Some(ps) = pools.get_mut(&addr) {
                    let block = state.current_block.load(Ordering::Relaxed);
                    ps.reserve_a    = res.reserve0 as u128;
                    ps.reserve_b    = res.reserve1 as u128;
                    ps.last_updated = block;
                }
            }
            Err(e) => {
                tracing::warn!(pool = %addr, error = %e, "Reconcile getReserves failed");
            }
        }
    }
    Ok(())
}

/// Register a new pool in BotState at startup.
/// Called once per known pool before the event loop starts.
pub async fn register_pool(
    http:      &HttpProvider,
    state:     &Arc<BotState>,
    pool_addr: Address,
    pool_type: PoolType,
    fee_bps:   u32,
) -> Result<()> {
    let pair = IUniV2Pair::new(pool_addr, http);

    let token_a = pair.token0().call().await?.token0;
    let token_b = pair.token1().call().await?.token1;
    let res     = pair.getReserves().call().await?;

    let ps = PoolState {
        reserve_a:    res.reserve0 as u128,
        reserve_b:    res.reserve1 as u128,
        token_a,
        token_b,
        fee_bps,
        pool_type,
        last_updated: 0, // will be set on first event
        amp: 0.0,        // set separately for stable pools
    };

    state.pool_states.write().insert(pool_addr, ps);
    tracing::info!(pool = %pool_addr, "Pool registered");
    Ok(())
}
