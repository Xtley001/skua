// crates/core/src/config.rs
//
// SKUA Production Law: Every capital-related value comes from environment.
// Missing variable → hard error. No substitution. No defaults.

use alloy::primitives::Address;
use anyhow::{Context, Result};
use std::env;
use std::str::FromStr;

/// Parses a required environment variable, returning a clear error if absent.
fn req<T>(key: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    let raw = env::var(key).with_context(|| format!("Missing required env var: {key}"))?;
    raw.parse()
        .map_err(|e| anyhow::anyhow!("Env var {key}={raw:?} failed to parse: {e}"))
}

/// Parses a required Address env var and asserts it is non-zero.
fn req_addr(key: &str) -> Result<Address> {
    let addr: Address = req(key)?;
    anyhow::ensure!(addr != Address::ZERO, "Env var {key} must not be zero address");
    Ok(addr)
}

/// Complete runtime configuration for SKUA.
/// Every field is required. No field has a compiled-in default.
#[derive(Debug, Clone)]
pub struct SkuaConfig {
    // ── Network ─────────────────────────────────────────────────────
    pub chain_id:         u64,
    pub rpc_ws_url:       String,
    pub rpc_http_url:     String,
    pub rpc_fallback_url: String,

    // ── Deployed contract addresses ─────────────────────────────────
    pub balancer_vault:        Address,
    pub hyperlend_pool:        Address,
    pub contract_evm_arb:      Address,
    pub contract_liquidation:  Address,
    pub contract_tri_arb:      Address,
    pub contract_stable_depeg: Address,
    pub profit_wallet:         Address,

    // ── HyperCore market/token indices ──────────────────────────────
    pub hype_market_index: u32,
    pub usdc_token_index:  u32,

    // ── Strategy minimum profit floors (USD) ────────────────────────
    pub s1_min_profit_usd: f64,
    pub s2_min_profit_usd: f64,
    pub s3_min_profit_usd: f64,
    pub s4_min_profit_usd: f64,

    // ── Signal thresholds ────────────────────────────────────────────
    pub s1_signal_threshold_bps: f64,
    pub s4_depeg_threshold_bps:  f64,

    // ── Capital ceilings / ROI ───────────────────────────────────────
    pub hard_cap_usd:       f64,
    pub gas_roi_multiplier: f64,

    // ── gasPrice multipliers (per strategy) ─────────────────────────
    pub gasprice_multiplier_arb:         u64,
    pub gasprice_multiplier_liquidation: u64,
    pub gasprice_multiplier_depeg:       u64,

    // ── Circuit breakers ────────────────────────────────────────────
    pub max_consecutive_reverts:  u32,
    pub gas_kill_threshold_hype:  f64,
    pub gas_kill_window_secs:     u64,

    // ── API / alerting ───────────────────────────────────────────────
    pub api_port:             u16,
    pub api_key:              String,
    pub telegram_bot_token:   String,
    pub telegram_chat_id:     String,
}

impl SkuaConfig {
    /// Load every field from the process environment.
    /// Returns `Err` immediately on the first missing or malformed variable.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            chain_id:         req("SKUA_CHAIN_ID")?,
            rpc_ws_url:       req("SKUA_RPC_WS_URL")?,
            rpc_http_url:     req("SKUA_RPC_HTTP_URL")?,
            rpc_fallback_url: req("SKUA_RPC_FALLBACK_URL")?,

            balancer_vault:        req_addr("SKUA_BALANCER_VAULT")?,
            hyperlend_pool:        req_addr("SKUA_HYPERLEND_POOL")?,
            contract_evm_arb:      req_addr("SKUA_CONTRACT_EVM_ARB")?,
            contract_liquidation:  req_addr("SKUA_CONTRACT_LIQ")?,
            contract_tri_arb:      req_addr("SKUA_CONTRACT_TRI")?,
            contract_stable_depeg: req_addr("SKUA_CONTRACT_DEPEG")?,
            profit_wallet:         req_addr("SKUA_PROFIT_WALLET")?,

            hype_market_index: req("SKUA_HYPE_MARKET_INDEX")?,
            usdc_token_index:  req("SKUA_USDC_TOKEN_INDEX")?,

            s1_min_profit_usd: req("SKUA_S1_MIN_PROFIT_USD")?,
            s2_min_profit_usd: req("SKUA_S2_MIN_PROFIT_USD")?,
            s3_min_profit_usd: req("SKUA_S3_MIN_PROFIT_USD")?,
            s4_min_profit_usd: req("SKUA_S4_MIN_PROFIT_USD")?,

            s1_signal_threshold_bps: req("SKUA_S1_SIGNAL_BPS")?,
            s4_depeg_threshold_bps:  req("SKUA_S4_DEPEG_BPS")?,

            hard_cap_usd:       req("SKUA_HARD_CAP_USD")?,
            gas_roi_multiplier: req("SKUA_GAS_ROI_MULTIPLIER")?,

            gasprice_multiplier_arb:         req("SKUA_GASPRICE_ARB")?,
            gasprice_multiplier_liquidation: req("SKUA_GASPRICE_LIQ")?,
            gasprice_multiplier_depeg:       req("SKUA_GASPRICE_DEPEG")?,

            max_consecutive_reverts: req("SKUA_MAX_CONSECUTIVE_REVERTS")?,
            gas_kill_threshold_hype: req("SKUA_GAS_KILL_HYPE")?,
            gas_kill_window_secs:    req("SKUA_GAS_KILL_WINDOW_SECS")?,

            api_port:           req("SKUA_API_PORT")?,
            api_key:            req("SKUA_API_KEY")?,
            telegram_bot_token: req("SKUA_TELEGRAM_BOT_TOKEN")?,
            telegram_chat_id:   req("SKUA_TELEGRAM_CHAT_ID")?,
        })
    }
}
