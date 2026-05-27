// crates/strategies/evm_arb/src/phase.rs
//
// AUDIT FIXES:
//   #5:  Phase2Pending now stores held_token so timeout can emergency exit
//   #15: Phase1Complete stores expected_profit_usd so the threshold is non-zero

use alloy::primitives::{Address, TxHash};
use std::time::{Duration, Instant};

pub const PHASE2_TIMEOUT: Duration = Duration::from_secs(30);

/// Fractional loss threshold: if unrealised loss exceeds this fraction of
/// expected profit, exit early.
pub const EMERGENCY_EXIT_LOSS_THRESHOLD: f64 = 0.5;

#[derive(Debug, Clone)]
pub enum S1Phase {
    Idle,

    /// Phase 1 complete — tokens held in Escrow.
    /// AUDIT FIX #15: stores expected_profit_usd (was 0.0, making exit fire immediately)
    /// AUDIT FIX #5:  stores held_token (needed for Phase2Pending → emergency exit)
    Phase1Complete {
        held_token:           Address,   // AUDIT FIX #5/#15: was Address::ZERO
        held_amount:          u128,
        min_phase2_proceeds:  u128,
        expected_profit_usd:  f64,       // AUDIT FIX #15: was missing
        entered_at:           Instant,
        phase1_tx:            TxHash,
    },

    /// CoreWriter order placed — waiting for fill confirmation.
    /// AUDIT FIX #5: now stores held_token so timeout handler can call emergencyExit
    Phase2Pending {
        held_token: Address,  // AUDIT FIX #5: was missing
        held_amount: u128,
        cloid:      u64,
        placed_at:  Instant,
    },

    Phase2Complete {
        proceeds_hype: f64,
    },

    EmergencyExited {
        swapped_out: u128,
        net_result:  i128,
    },
}

impl S1Phase {
    /// Returns true if an emergency exit should be triggered.
    /// AUDIT FIX #15: uses stored expected_profit_usd (no longer 0.0)
    pub fn should_emergency_exit(
        &self,
        current_evm_price:   f64,
        expected_sell_price: f64,
    ) -> bool {
        if let S1Phase::Phase1Complete {
            entered_at,
            expected_profit_usd,
            held_amount,
            ..
        } = self {
            // Timeout check
            if entered_at.elapsed() >= PHASE2_TIMEOUT {
                return true;
            }

            // Adverse price move check — only meaningful when expected_profit > 0
            if *expected_profit_usd > 0.0 && expected_sell_price > 0.0 {
                let price_move_frac =
                    (expected_sell_price - current_evm_price) / expected_sell_price;
                // Convert fractional move to USD loss based on position size
                let position_usd = (*held_amount as f64) / 1e8 * expected_sell_price;
                let loss_usd = price_move_frac.max(0.0) * position_usd;
                if loss_usd > expected_profit_usd * EMERGENCY_EXIT_LOSS_THRESHOLD {
                    return true;
                }
            }
        }
        false
    }
}

/// S1 risk check: only execute if expected profit > 2× estimated inter-phase move.
pub fn s1_risk_check(
    expected_profit_usd:  f64,
    position_size_usd:    f64,
    vol_per_second:       f64,
    inter_phase_seconds:  f64,
) -> bool {
    let estimated_move = position_size_usd * vol_per_second * inter_phase_seconds;
    expected_profit_usd > 2.0 * estimated_move
}

/// Rolling volatility estimator.
pub struct VolEstimator {
    prices:   Vec<f64>,
    capacity: usize,
    head:     usize,
    count:    usize,
}

impl VolEstimator {
    pub fn new(window: usize) -> Self {
        Self { prices: vec![0.0; window], capacity: window, head: 0, count: 0 }
    }

    pub fn push(&mut self, price: f64) {
        self.prices[self.head] = price;
        self.head = (self.head + 1) % self.capacity;
        if self.count < self.capacity { self.count += 1; }
    }

    pub fn vol_per_second(&self) -> f64 {
        if self.count < 2 { return 0.0; }
        let n = self.count as f64;
        let slice = &self.prices[..self.count];
        let mean = slice.iter().sum::<f64>() / n;
        let variance = slice.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1.0);
        variance.sqrt() / self.count as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::TxHash;

    #[test]
    fn emergency_exit_fires_on_timeout() {
        let phase = S1Phase::Phase1Complete {
            held_token:          Address::ZERO,
            held_amount:         100_000_000,
            min_phase2_proceeds: 90_000_000,
            expected_profit_usd: 50.0,
            entered_at:          Instant::now() - PHASE2_TIMEOUT - Duration::from_secs(1),
            phase1_tx:           TxHash::ZERO,
        };
        assert!(phase.should_emergency_exit(100.0, 101.0));
    }

    #[test]
    fn emergency_exit_does_not_fire_immediately_with_real_profit() {
        // AUDIT FIX #15 regression: with expected_profit > 0 and no price move,
        // should_emergency_exit must NOT fire immediately
        let phase = S1Phase::Phase1Complete {
            held_token:          Address::ZERO,
            held_amount:         100_000_000,
            min_phase2_proceeds: 90_000_000,
            expected_profit_usd: 50.0,
            entered_at:          Instant::now(),
            phase1_tx:           TxHash::ZERO,
        };
        // Price stable — no move, no timeout
        assert!(!phase.should_emergency_exit(100.0, 100.0));
    }

    #[test]
    fn risk_check_passes_high_profit() {
        assert!(s1_risk_check(100.0, 10_000.0, 0.001, 5.0));
    }

    #[test]
    fn risk_check_fails_low_profit() {
        assert!(!s1_risk_check(5.0, 10_000.0, 0.01, 5.0));
    }

    #[test]
    fn vol_estimator_zero_on_flat() {
        let mut v = VolEstimator::new(60);
        for _ in 0..10 { v.push(100.0); }
        assert_eq!(v.vol_per_second(), 0.0);
    }
}
