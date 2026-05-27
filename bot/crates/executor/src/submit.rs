// crates/executor/src/submit.rs
//
// AUDIT FIX #23: chain_id read from SkuaConfig — not hardcoded 999.
// eth_sendRawTransactionSync is the ONLY submission method.

use anyhow::{anyhow, Context, Result};
use alloy::network::EthereumWallet;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use skua_core::{BotState, SkuaConfig};
use skua_chain::HttpProvider;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::wallet_pool::WalletPool;

/// Build and sign a raw transaction.
/// AUDIT FIX #23: chain_id comes from config, not hardcoded.
pub async fn build_raw_tx(
    provider:  &HttpProvider,
    wallet:    &EthereumWallet,
    to:        Address,
    calldata:  Bytes,
    nonce:     u64,
    gas_limit: u64,
    gas_price: u128,
    chain_id:  u64,            // AUDIT FIX #23: was hardcoded 999
) -> Result<Bytes> {
    let tx = TransactionRequest::default()
        .to(to)
        .input(calldata.into())
        .nonce(nonce)
        .gas_limit(gas_limit)
        .gas_price(gas_price)
        .chain_id(chain_id);   // AUDIT FIX #23: from config

    let envelope = tx
        .build(wallet)
        .await
        .context("Failed to sign transaction")?;

    Ok(alloy::rlp::encode(&envelope).into())
}

/// Submit via eth_sendRawTransactionSync.
/// Checks kill switch and circuit breaker before every submission.
/// Updates consecutive_reverts and gas_spent_window on result.
pub async fn submit_sync(
    provider: &HttpProvider,
    raw_tx:   Bytes,
    state:    &Arc<BotState>,
    config:   &SkuaConfig,
) -> Result<TransactionReceipt> {
    if state.kill_switch.load(Ordering::SeqCst) {
        return Err(anyhow!("Kill switch active — submission blocked"));
    }

    let reverts = state.consecutive_reverts.load(Ordering::SeqCst);
    if reverts >= config.max_consecutive_reverts {
        state.halt("Circuit breaker: consecutive revert threshold exceeded");
        return Err(anyhow!("Circuit breaker fired: {reverts} consecutive reverts"));
    }

    let result: serde_json::Value = provider
        .raw_request("eth_sendRawTransactionSync".into(), (raw_tx,))
        .await
        .context("eth_sendRawTransactionSync failed")?;

    let receipt: TransactionReceipt = serde_json::from_value(result)
        .context("Failed to deserialize sync receipt")?;

    match receipt.status() {
        true => {
            state.record_success();
            tracing::debug!(
                tx_hash  = %receipt.transaction_hash,
                gas_used = receipt.gas_used,
                "Transaction landed"
            );
        }
        false => {
            let gas_scaled = gas_cost_hype_scaled(&receipt, state);
            let new_count  = state.record_revert(gas_scaled);

            tracing::warn!(
                tx_hash      = %receipt.transaction_hash,
                consecutive  = new_count,
                max          = config.max_consecutive_reverts,
                "Transaction reverted"
            );

            let gas_spent = state.gas_spent_window.load(Ordering::Relaxed) as f64 / 1e8;
            if gas_spent >= config.gas_kill_threshold_hype {
                state.halt(&format!(
                    "Gas kill threshold: {gas_spent:.4} HYPE spent in window"
                ));
            }

            if new_count >= config.max_consecutive_reverts {
                state.halt(&format!("Circuit breaker: {new_count} consecutive reverts"));
            }
        }
    }

    Ok(receipt)
}

fn gas_cost_hype_scaled(receipt: &TransactionReceipt, state: &Arc<BotState>) -> u64 {
    let gas_used     = receipt.gas_used as u64;
    let base_fee_wei = state.current_base_fee.load(Ordering::Relaxed);
    let cost_wei     = gas_used.saturating_mul(base_fee_wei);
    (cost_wei / 10_000_000_000) as u64
}
