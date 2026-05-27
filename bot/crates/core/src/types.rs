// crates/core/src/types.rs — shared domain types

use alloy::primitives::Address;

/// Which flash loan provider to use for a given opportunity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashProvider {
    Balancer,   // prefer: lower/zero fee
    HyperLend,  // fallback
}

/// Available liquidity from a flash provider for a token, in USD.
#[derive(Debug, Clone)]
pub struct FlashLiquidity {
    pub provider:       FlashProvider,
    pub available_usd:  f64,
    pub token:          Address,
}

/// A block event dispatched to strategy evaluators.
#[derive(Debug, Clone)]
pub enum BlockEvent {
    NewBlock(u64),
    PoolUpdated(Address),
    PositionUpdated(Address), // HyperLend borrower address
}

/// Price map: token address → USD price (from HyperCore oracle precompile)
pub type PriceMap = std::collections::HashMap<Address, f64>;

/// Result of the algebraic sizing pass.
#[derive(Debug, Clone)]
pub struct SizingResult {
    pub optimal_amount_wei: u128,
    pub optimal_amount_usd: f64,
    pub expected_profit_usd: f64,
    pub flash_provider: FlashProvider,
}

/// Result of the eth_call simulation gate.
#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub success:    bool,
    pub profit_usd: f64,
}

/// Per-strategy performance counters (for Prometheus).
#[derive(Debug, Default)]
pub struct StrategyCounters {
    pub opps_detected:  u64,
    pub sims_passed:    u64,
    pub txs_submitted:  u64,
    pub txs_landed:     u64,
    pub txs_reverted:   u64,
    pub profit_usd:     f64,
    pub gas_hype:       f64,
}
