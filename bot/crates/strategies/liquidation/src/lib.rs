// crates/strategies/liquidation/src/lib.rs
//
// S2: HyperLend liquidations — the most reliable SKUA strategy.
// Protocol-enforced liquidation bonus. Fully atomic. No CoreWriter.
// Uses the same HyperCore oracle precompile that HyperLend itself uses.

pub mod monitor;
pub mod sizing;
pub mod executor;
