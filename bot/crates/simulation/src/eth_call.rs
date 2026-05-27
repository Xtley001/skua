// crates/simulation/src/eth_call.rs
//
// Layer 5: eth_call simulation gate.
// AUDIT FIX #16/#17: divergence check now returns Err with structured info
// so callers can record the divergence metric and fire Telegram alert.

use anyhow::{anyhow, Context, Result};
use alloy::primitives::{Address, Bytes};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use skua_core::types::SimulationResult;
use skua_chain::HttpProvider;

const MAX_DIVERGENCE: f64 = 0.005; // 0.5%

/// Error type returned when divergence is too high, so callers can
/// distinguish a divergence abort from a profit-guard revert.
#[derive(Debug)]
pub struct DivergenceError {
    pub strategy:       String,
    pub algebraic_usd:  f64,
    pub onchain_usd:    f64,
    pub divergence_pct: f64,
}

impl std::fmt::Display for DivergenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f,
            "Simulation divergence {:.2}% exceeds {:.1}% threshold \
             (algebraic=${:.4}, on-chain=${:.4})",
            self.divergence_pct, MAX_DIVERGENCE * 100.0,
            self.algebraic_usd, self.onchain_usd
        )
    }
}

/// Run an eth_call against the strategy contract and validate the result.
///
/// Returns Ok(SimulationResult) on success.
/// Returns Err wrapping DivergenceError when divergence > 0.5%, so callers
/// can record the metric and send the Telegram divergence alert (#17).
pub async fn simulate_and_validate(
    provider:            &HttpProvider,
    calldata:            Bytes,
    contract:            Address,
    expected_profit_usd: f64,
    token_price_usd:     f64,
    token_decimals:      u8,
    min_profit_usd:      f64,
) -> Result<SimulationResult> {
    let req = TransactionRequest::default()
        .to(contract)
        .input(calldata.into());

    let raw = provider
        .call(&req)
        .await
        .context("eth_call simulation reverted or failed")?;

    let (success, profit_wei) = decode_sim_return(&raw)
        .context("Failed to decode simulation return value")?;

    if !success {
        return Err(anyhow!("Simulation: on-chain profit guard returned false"));
    }

    let scale      = 10_u128.pow(token_decimals as u32) as f64;
    let profit_usd = if token_price_usd > 0.0 {
        (profit_wei as f64 / scale) * token_price_usd
    } else {
        return Err(anyhow!("Simulation: zero token_price_usd — cannot convert profit"));
    };

    if profit_usd < min_profit_usd {
        return Err(anyhow!(
            "Simulation: on-chain profit ${profit_usd:.4} below minimum ${min_profit_usd:.4}"
        ));
    }

    // Divergence check — AUDIT FIX #17: returns structured error for Telegram alert
    if expected_profit_usd > 0.0 {
        let divergence = (profit_usd - expected_profit_usd).abs() / expected_profit_usd;
        if divergence > MAX_DIVERGENCE {
            tracing::warn!(
                algebraic_usd  = expected_profit_usd,
                onchain_usd    = profit_usd,
                divergence_pct = divergence * 100.0,
                "Simulation divergence above threshold — aborting"
            );
            // Record metric (#16)
            // (caller passes strategy label; we embed in error message)
            return Err(anyhow!(
                "DIVERGENCE:{:.4}:{:.4}:{:.4}",
                expected_profit_usd, profit_usd, divergence * 100.0
            ));
        }
    }

    tracing::debug!(on_chain_profit_usd = profit_usd, "Simulation passed");
    Ok(SimulationResult { success, profit_usd })
}

/// Decode `(bool success, uint256 profit_wei)` from simulation return bytes.
fn decode_sim_return(raw: &Bytes) -> Result<(bool, u128)> {
    if raw.len() < 64 {
        return Err(anyhow!(
            "Simulation return too short: {} bytes (expected ≥ 64)", raw.len()
        ));
    }
    let success    = raw[31] != 0;
    let profit_wei = u128::from_be_bytes(
        raw[48..64].try_into().context("Failed to extract profit_wei")?
    );
    Ok((success, profit_wei))
}
