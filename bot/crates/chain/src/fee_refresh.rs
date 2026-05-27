// crates/chain/src/fee_refresh.rs
//
// Flash loan fees are governance-adjustable.
// HyperLend fee: read FLASHLOAN_PREMIUM_TOTAL() from the pool contract.
// Balancer V3 fee: read getProtocolFeePercentages() from the vault.
//
// Refresh every 1,000 blocks. Never hardcode either fee.
// Failure to read is a hard error — propagate it.

use anyhow::{Context, Result};
use skua_core::{BotState, SkuaConfig};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use alloy::sol;

use super::providers::HttpProvider;

// ── Solidity ABI bindings (minimal, via alloy::sol!) ─────────────────────────

sol! {
    #[allow(missing_docs)]
    interface IHyperLendPool {
        function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);
    }

    #[allow(missing_docs)]
    interface IBalancerVault {
        function getProtocolFeePercentages()
            external
            view
            returns (
                uint256 swapFeePercentage,
                uint256 flashLoanFeePercentage,
                uint256 yieldFeePercentage
            );
    }
}

/// Background loop: re-reads both flash loan fees every 1,000 blocks.
/// Runs until the process exits. Errors are logged but do NOT kill the loop
/// (a transient RPC hiccup must not stop the bot from running).
pub async fn fee_refresh_loop(
    state:  Arc<BotState>,
    http:   HttpProvider,
    config: SkuaConfig,
) {
    let mut last_refresh_block: u64 = 0;

    loop {
        sleep(Duration::from_secs(1)).await;

        let block = state.current_block.load(Ordering::Relaxed);

        // Refresh on startup (last_refresh == 0) and every 1,000 blocks thereafter
        if block < last_refresh_block + 1_000 && last_refresh_block != 0 {
            continue;
        }

        match refresh_fees(&state, &http, &config).await {
            Ok((hl_bps, bal_bps)) => {
                tracing::info!(
                    block,
                    hyperlend_fee_bps = hl_bps,
                    balancer_fee_bps  = bal_bps,
                    "Fee refresh complete"
                );
                last_refresh_block = block;
            }
            Err(e) => {
                tracing::error!(error = %e, "Fee refresh failed — will retry next block");
            }
        }
    }
}

/// Perform one fee refresh cycle. Returns (hyperlend_bps, balancer_bps) on success.
async fn refresh_fees(
    state:  &Arc<BotState>,
    http:   &HttpProvider,
    config: &SkuaConfig,
) -> Result<(u64, u64)> {
    // ── HyperLend flash fee ──────────────────────────────────────────────
    let hl_contract = IHyperLendPool::new(config.hyperlend_pool, http);
    let hl_raw = hl_contract
        .FLASHLOAN_PREMIUM_TOTAL()
        .call()
        .await
        .context("Failed to read HyperLend FLASHLOAN_PREMIUM_TOTAL")?
        ._0; // uint128

    // HyperLend stores the fee already in bps (e.g. 4 = 0.04%)
    let hl_bps = hl_raw as u64;
    state.hyperlend_fee_bps.store(hl_bps, Ordering::SeqCst);

    // ── Balancer V3 flash fee ────────────────────────────────────────────
    let bal_contract = IBalancerVault::new(config.balancer_vault, http);
    let bal_result = bal_contract
        .getProtocolFeePercentages()
        .call()
        .await
        .context("Failed to read Balancer V3 protocol fee")?;

    // flashLoanFeePercentage is a WAD (10^18 = 100%)
    // Convert to bps: fee_bps = fee_wad * 10_000 / 1e18
    let bal_wad: u128 = bal_result.flashLoanFeePercentage.try_into()
        .context("Balancer fee WAD overflowed u128")?;
    let bal_bps = (bal_wad * 10_000 / 1_000_000_000_000_000_000u128) as u64;
    state.balancer_fee_bps.store(bal_bps, Ordering::SeqCst);

    Ok((hl_bps, bal_bps))
}
