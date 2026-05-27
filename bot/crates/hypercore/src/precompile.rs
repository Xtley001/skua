// crates/hypercore/src/precompile.rs
//
// SKUA Law 1: Precompile values are block-scoped.
// HyperCore prices change every block. Never cache across block boundaries.
// Every call reads the PREVIOUS block's state — operationally current on a 1s chain.
//
// L1READ base address: 0x0000000000000000000000000000000000000800 (immutable constant)

use anyhow::{anyhow, Context, Result};
use alloy::primitives::{address, Address};
use alloy::sol;
use skua_core::BotState;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use skua_chain::HttpProvider;

/// L1Read precompile address — immutable constant, never stored in a variable slot.
pub const L1READ_ADDRESS: Address =
    address!("0000000000000000000000000000000000000800");

sol! {
    #[allow(missing_docs)]
    interface IL1Read {
        /// Best mid price from order book (previous block state).
        /// Returns price as fixed-point integer: human_price × 10^8.
        function spotPx(uint32 marketIndex) external view returns (uint64);

        /// Oracle/mark price (smoothed — use for liquidation health factor check).
        function oraclePx(uint32 tokenIndex) external view returns (uint64);

        /// HyperCore spot balance of an address.
        function spotBalance(address user, uint32 tokenIndex) external view returns (uint64);

        /// Perp position: signed size and entry notional.
        function perpPosition(address user, uint32 marketIndex)
            external view
            returns (int64 szi, uint64 entryNtl);

        /// Current perp funding rate (signed, per hour).
        function fundingRate(uint32 marketIndex) external view returns (int64);
    }
}

/// Read all required precompile data in a single batch.
/// Returns Err if any value is zero (see production law: no Ok(0) in hot path).
pub struct PrecompileReader {
    contract: IL1Read::IL1ReadInstance<(), HttpProvider>,
}

impl PrecompileReader {
    pub fn new(http: HttpProvider) -> Self {
        Self {
            contract: IL1Read::new(L1READ_ADDRESS, http),
        }
    }

    /// Read spot price for a market index.
    /// Returns Err if the returned price is zero.
    pub async fn spot_px(&self, market_index: u32) -> Result<u64> {
        let price = self
            .contract
            .spotPx(market_index)
            .call()
            .await
            .context("Failed to read spotPx from L1Read precompile")?
            ._0;

        if price == 0 {
            return Err(anyhow!(
                "L1Read.spotPx({market_index}) returned 0 — precompile unreliable"
            ));
        }
        Ok(price)
    }

    /// Read oracle/mark price for a token index.
    /// Returns Err if the returned price is zero.
    pub async fn oracle_px(&self, token_index: u32) -> Result<u64> {
        let price = self
            .contract
            .oraclePx(token_index)
            .call()
            .await
            .context("Failed to read oraclePx from L1Read precompile")?
            ._0;

        if price == 0 {
            return Err(anyhow!(
                "L1Read.oraclePx({token_index}) returned 0 — precompile unreliable"
            ));
        }
        Ok(price)
    }

    /// Read the HyperCore spot balance of a user for a token index.
    pub async fn spot_balance(&self, user: Address, token_index: u32) -> Result<u64> {
        Ok(self
            .contract
            .spotBalance(user, token_index)
            .call()
            .await
            .context("Failed to read spotBalance from L1Read precompile")?
            ._0)
    }

    /// Read funding rate for a perp market.
    pub async fn funding_rate(&self, market_index: u32) -> Result<i64> {
        Ok(self
            .contract
            .fundingRate(market_index)
            .call()
            .await
            .context("Failed to read fundingRate from L1Read precompile")?
            ._0)
    }
}

/// Refresh the HYPE/USD price in BotState from the L1Read oracle precompile.
///
/// Called every block as a background task.
/// Does NOT update state if price is zero — a zero price would corrupt all
/// profit calculations.
pub async fn refresh_hype_price(
    state:             Arc<BotState>,
    reader:            &PrecompileReader,
    hype_market_index: u32,
) -> Result<()> {
    let price_raw = reader.oracle_px(hype_market_index).await?;
    // price_raw is already confirmed non-zero by oracle_px()
    state.hype_price_usd.store(price_raw, Ordering::SeqCst);
    Ok(())
}

/// Convert a precompile price (scaled 10^8) to a human-readable f64 USD value.
#[inline]
pub fn precompile_price_to_f64(raw: u64) -> f64 {
    raw as f64 / 1e8
}
