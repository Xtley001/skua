// crates/core/src/state.rs
//
// BotState is the single shared runtime object threaded through every subsystem.
// All fields are either atomic primitives (lock-free reads) or Arc<RwLock<_>>.
// Pool states and HyperLend positions are refreshed on every relevant event.
// No field is ever read from a stale cross-block cache.

use alloy::primitives::Address;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc,
};

// ── Pool classification ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum PoolType {
    BalancerV3StablePool,
    BalancerV3WeightedPool,
    UniV3,
    ConstantProduct, // generic Uni V2-style
}

// ── Pool state — always the live reserve state, never stale ─────────────────

#[derive(Debug, Clone)]
pub struct PoolState {
    pub reserve_a:    u128,
    pub reserve_b:    u128,
    pub token_a:      Address,
    pub token_b:      Address,
    pub fee_bps:      u32,       // read from pool contract at startup, refreshed
    pub pool_type:    PoolType,
    pub last_updated: u64,       // block number of last update
    /// Stable pool amplification coefficient (A). 0 for non-stable pools.
    pub amp:          f64,
}

// ── HyperLend position ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CollateralAsset {
    pub token:     Address,
    pub amount:    u128,
    /// liquidationThreshold (NOT just LTV) — read from protocol reserve config
    pub ltv:       f64,
    /// Liquidation bonus (e.g. 1.05 = 5%) — read from protocol reserve config
    pub liq_bonus: f64,
    pub decimals:  u8,
}

#[derive(Debug, Clone)]
pub struct DebtAsset {
    pub token:    Address,
    pub amount:   u128, // variable + stable debt
    pub decimals: u8,
}

#[derive(Debug, Clone)]
pub struct BorrowerPosition {
    pub address:           Address,
    pub collateral_assets: Vec<CollateralAsset>,
    pub debt_assets:       Vec<DebtAsset>,
    pub computed_hf:       f64,  // recomputed every block using precompile prices
    pub last_block:        u64,
}

// ── Order book snapshot (S1) ─────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct OrderBook {
    pub coin:     String,
    pub best_bid: f64,
    pub best_ask: f64,
    pub bid_depth: Vec<(f64, f64)>, // (price, size) sorted best-first
    pub ask_depth: Vec<(f64, f64)>,
    pub last_updated_ms: u64,
}

// ── Wallet pool (forward declaration — full impl in executor crate) ──────────

pub struct WalletPool; // placeholder; real impl lives in executor crate

// ── BotState ─────────────────────────────────────────────────────────────────

pub struct BotState {
    // ── Live chain data — refreshed every block ──────────────────────────
    pub current_block:    AtomicU64,
    pub current_base_fee: AtomicU64, // in wei
    /// hype_price_usd scaled 10^8 (i.e. $10.00 → stored as 1_000_000_000)
    pub hype_price_usd:   AtomicU64,
    pub last_block_time:  AtomicU64, // unix timestamp ms

    // ── Flash loan fees — refreshed every 1,000 blocks ──────────────────
    /// Balancer V3 flash loan fee percentage. Stored as bps × 10^4.
    pub balancer_fee_bps:  AtomicU64,
    /// HyperLend FLASHLOAN_PREMIUM_TOTAL. Stored as bps × 10^4.
    pub hyperlend_fee_bps: AtomicU64,

    // ── Pool state — updated on every Sync/Swap/PoolBalanceChanged event ─
    pub pool_states: Arc<RwLock<HashMap<Address, PoolState>>>,

    // ── HyperLend positions — event-driven + 10-block reconciliation ─────
    pub hyperlend_positions: Arc<RwLock<HashMap<Address, BorrowerPosition>>>,

    // ── HyperCore order books — updated via WS push ───────────────────────
    pub orderbooks: Arc<RwLock<HashMap<String, OrderBook>>>,

    // ── Strategy enable toggles ───────────────────────────────────────────
    pub s1_enabled: AtomicBool,
    pub s2_enabled: AtomicBool,
    pub s3_enabled: AtomicBool,
    pub s4_enabled: AtomicBool,

    // ── Circuit breakers ─────────────────────────────────────────────────
    /// true = halt all submissions immediately
    pub kill_switch: AtomicBool,
    /// increments on every revert; reset to 0 on success; fires breaker when > threshold
    pub consecutive_reverts: AtomicU32,
    /// HYPE spent in rolling window, scaled 10^8
    pub gas_spent_window: AtomicU64,
}

impl BotState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            current_block:    AtomicU64::new(0),
            current_base_fee: AtomicU64::new(0),
            hype_price_usd:   AtomicU64::new(0),
            last_block_time:  AtomicU64::new(0),

            balancer_fee_bps:  AtomicU64::new(0),
            hyperlend_fee_bps: AtomicU64::new(0),

            pool_states:          Arc::new(RwLock::new(HashMap::new())),
            hyperlend_positions:  Arc::new(RwLock::new(HashMap::new())),
            orderbooks:           Arc::new(RwLock::new(HashMap::new())),

            s1_enabled: AtomicBool::new(false),
            s2_enabled: AtomicBool::new(false),
            s3_enabled: AtomicBool::new(false),
            s4_enabled: AtomicBool::new(false),

            kill_switch:         AtomicBool::new(false),
            consecutive_reverts: AtomicU32::new(0),
            gas_spent_window:    AtomicU64::new(0),
        })
    }

    // ── Convenience accessors ────────────────────────────────────────────

    /// Returns the HYPE price in USD as f64.
    /// Stored as integer scaled 10^8; divide back out.
    pub fn hype_price_f64(&self) -> f64 {
        self.hype_price_usd.load(Ordering::Relaxed) as f64 / 1e8
    }

    /// Returns the current base fee in wei as u128.
    pub fn base_fee_wei(&self) -> u128 {
        self.current_base_fee.load(Ordering::Relaxed) as u128
    }

    /// Returns true if the kill switch is active OR the consecutive revert
    /// threshold has been exceeded.
    pub fn is_halted(&self, max_reverts: u32) -> bool {
        self.kill_switch.load(Ordering::SeqCst)
            || self.consecutive_reverts.load(Ordering::SeqCst) >= max_reverts
    }

    /// Activate kill switch unconditionally and log the reason.
    pub fn halt(&self, reason: &str) {
        self.kill_switch.store(true, Ordering::SeqCst);
        tracing::error!(reason, "KILL SWITCH ACTIVATED");
    }

    /// Record a successful strategy execution.
    pub fn record_success(&self) {
        self.consecutive_reverts.store(0, Ordering::SeqCst);
    }

    /// Record a transaction revert. Returns new revert count.
    pub fn record_revert(&self, gas_hype_scaled: u64) -> u32 {
        let count = self.consecutive_reverts.fetch_add(1, Ordering::SeqCst) + 1;
        self.gas_spent_window
            .fetch_add(gas_hype_scaled, Ordering::Relaxed);
        count
    }
}

impl Default for BotState {
    fn default() -> Self {
        Arc::try_unwrap(BotState::new()).expect("only reference")
    }
}
