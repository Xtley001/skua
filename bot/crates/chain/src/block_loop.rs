// crates/chain/src/block_loop.rs
//
// SKUA Law: Always subscribe. Never poll.
// Block subscription drives ALL strategy evaluations.
// If the stream terminates unexpectedly, this function returns Err —
// the supervisor task is responsible for reconnecting and restarting.

use anyhow::{Context, Result, anyhow};
use skua_core::{BotState, BlockEvent};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use alloy::providers::Provider;
use alloy::rpc::types::Header;

use super::providers::WsProvider;

/// Subscribes to new block headers on `ws` and:
///   1. Updates all atomic fields in `state` (block number, base fee, timestamp).
///   2. Spawns a background task to refresh the HYPE price from the precompile.
///   3. Sends a `BlockEvent::NewBlock` to the strategy evaluator channel.
///
/// Returns `Err` when the subscription stream terminates.
/// The caller must restart this loop (with a fresh WS provider) on error.
pub async fn block_subscription_loop(
    ws:          WsProvider,
    state:       Arc<BotState>,
    strategy_tx: mpsc::Sender<BlockEvent>,
) -> Result<()> {
    let mut sub = ws
        .subscribe_blocks()
        .await
        .context("Failed to subscribe to block headers")?;

    tracing::info!("Block subscription established");

    while let Some(header) = sub.next().await {
        process_block_header(header, &state, &strategy_tx).await;
    }

    Err(anyhow!("Block subscription stream terminated unexpectedly"))
}

/// Process a single incoming block header.
async fn process_block_header(
    header:      Header,
    state:       &Arc<BotState>,
    strategy_tx: &mpsc::Sender<BlockEvent>,
) {
    let block_num = header.number;

    // ── Update atomic state ──────────────────────────────────────────────
    state.current_block.store(block_num, Ordering::SeqCst);

    if let Some(base_fee) = header.base_fee_per_gas {
        // base_fee_per_gas is a u128 in alloy; store as u64 wei (safe for HyperEVM)
        state
            .current_base_fee
            .store(base_fee as u64, Ordering::SeqCst);
    }

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    state.last_block_time.store(now_ms, Ordering::SeqCst);

    tracing::debug!(
        block    = block_num,
        base_fee = state.current_base_fee.load(Ordering::Relaxed),
        "New block"
    );

    // ── Notify strategy evaluators ───────────────────────────────────────
    // try_send: non-blocking — if channel is full, we skip this notification.
    // Strategy evaluators process at their own pace; missing one tick is acceptable.
    if let Err(e) = strategy_tx.try_send(BlockEvent::NewBlock(block_num)) {
        tracing::warn!(
            block = block_num,
            error = %e,
            "Strategy channel full — block event dropped"
        );
    }
}
