// crates/scanner/src/s1_signal.rs
//
// S1 signal: detect price gap between EVM AMM and HyperCore order book.
//
// Two directions:
//   BuyEvmSellCore:       EVM price < HyperCore bid → buy on EVM, sell on Core
//   BuyCoreEscrowSellEvm: HyperCore ask < EVM price → buy on Core, sell on EVM
//
// Signal fires when |gap| > threshold_bps (from config, starts at 60 bps).
// Zero prices on either side: never act.

use std::sync::atomic::{AtomicU64, Ordering};

/// S1 signal direction.
#[derive(Debug, Clone)]
pub enum S1Signal {
    /// EVM price is below HyperCore bid — buy EVM, sell on Core (Phase 2)
    BuyEvmSellCore { delta_bps: f64 },
    /// HyperCore ask is below EVM price — buy on Core via escrow, sell on EVM
    BuyCoreEscrowSellEvm { delta_bps: f64 },
}

/// In-memory signal detector for S1.
/// All fields are atomic for lock-free reads from multiple strategy tasks.
pub struct S1Detector {
    /// Best bid on HyperCore — updated via WS push (scaled 10^8)
    pub core_bid: AtomicU64,
    /// Best ask on HyperCore — updated via WS push (scaled 10^8)
    pub core_ask: AtomicU64,
    /// EVM AMM mid price — updated on every Sync event (scaled 10^8)
    pub evm_mid:  AtomicU64,
    /// Threshold in bps from config
    pub threshold_bps: f64,
}

impl S1Detector {
    pub fn new(threshold_bps: f64) -> Self {
        Self {
            core_bid:      AtomicU64::new(0),
            core_ask:      AtomicU64::new(0),
            evm_mid:       AtomicU64::new(0),
            threshold_bps,
        }
    }

    /// Evaluate current prices for an S1 signal.
    /// Returns None if any price is zero — never act on zero prices.
    pub fn evaluate(&self) -> Option<S1Signal> {
        let bid   = self.core_bid.load(Ordering::Relaxed) as f64;
        let ask   = self.core_ask.load(Ordering::Relaxed) as f64;
        let price = self.evm_mid.load(Ordering::Relaxed) as f64;

        // Never act on zero prices — production law
        if price <= 0.0 || bid <= 0.0 || ask <= 0.0 {
            return None;
        }

        // Direction 1: buy cheap on EVM, sell at HyperCore bid
        let buy_evm_sell_core = (bid - price) / price * 10_000.0;
        if buy_evm_sell_core > self.threshold_bps {
            return Some(S1Signal::BuyEvmSellCore {
                delta_bps: buy_evm_sell_core,
            });
        }

        // Direction 2: buy at HyperCore ask (via Escrow), sell on EVM
        let buy_core_sell_evm = (price - ask) / price * 10_000.0;
        if buy_core_sell_evm > self.threshold_bps {
            return Some(S1Signal::BuyCoreEscrowSellEvm {
                delta_bps: buy_core_sell_evm,
            });
        }

        None
    }

    /// Update the HyperCore best bid (called from WS order book handler).
    pub fn update_core_bid(&self, bid_scaled: u64) {
        self.core_bid.store(bid_scaled, Ordering::Relaxed);
    }

    /// Update the HyperCore best ask (called from WS order book handler).
    pub fn update_core_ask(&self, ask_scaled: u64) {
        self.core_ask.store(ask_scaled, Ordering::Relaxed);
    }

    /// Update the EVM AMM mid price (called on Sync event).
    /// mid = reserve_out / reserve_in (scaled 10^8)
    pub fn update_evm_mid(&self, mid_scaled: u64) {
        self.evm_mid.store(mid_scaled, Ordering::Relaxed);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn detector(threshold_bps: f64) -> S1Detector {
        S1Detector::new(threshold_bps)
    }

    #[test]
    fn no_signal_when_prices_equal() {
        let d = detector(60.0);
        d.update_evm_mid(100_000_000_00); // $100
        d.update_core_bid(100_000_000_00);
        d.update_core_ask(100_000_000_00);
        assert!(d.evaluate().is_none());
    }

    #[test]
    fn no_signal_on_zero_price() {
        let d = detector(60.0);
        d.update_evm_mid(0);
        d.update_core_bid(100_000_000_00);
        d.update_core_ask(100_000_000_00);
        assert!(d.evaluate().is_none());
    }

    #[test]
    fn fires_buy_evm_sell_core_above_threshold() {
        let d = detector(60.0);
        // EVM price = 100, Core bid = 101 → delta = 100 bps
        d.update_evm_mid(10_000_000_000); // $100 × 10^8
        d.update_core_bid(10_100_000_000); // $101 × 10^8
        d.update_core_ask(10_100_000_000);
        match d.evaluate() {
            Some(S1Signal::BuyEvmSellCore { delta_bps }) => {
                assert!((delta_bps - 100.0).abs() < 0.1, "delta_bps = {delta_bps}");
            }
            other => panic!("Unexpected: {other:?}"),
        }
    }

    #[test]
    fn no_signal_below_threshold() {
        let d = detector(60.0);
        // 30 bps gap — below 60 bps threshold
        d.update_evm_mid(10_000_000_000);
        d.update_core_bid(10_030_000_000);
        d.update_core_ask(10_030_000_000);
        assert!(d.evaluate().is_none());
    }
}
