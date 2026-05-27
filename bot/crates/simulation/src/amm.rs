// crates/simulation/src/amm.rs
//
// AMM output functions must be algebraically exact.
// Two pool types:
//   1. Constant-product (Uni V2-style): exact closed-form formula
//   2. StableSwap (Curve invariant): Newton-Raphson, fee on OUTPUT (Curve spec)
//
// AUDIT FIXES:
//   F-10 variant: stable pool fee now applied to output, not input (audit finding #11)
//   F-13: both D and Y loops now use RELATIVE convergence (audit finding #10)

#[derive(Debug, Clone)]
pub struct AmmPool {
    pub reserve_in:  f64,
    pub reserve_out: f64,
    pub fee_bps:     f64,
    pub amp:         f64,
    pub n_tokens:    usize,
}

impl AmmPool {
    // ── Constant-product output ──────────────────────────────────────────

    /// dy = y × dx × (1-fee) / (x + dx × (1-fee))
    /// Fee applied on input — correct for Uniswap V2 convention.
    pub fn get_output(&self, amount_in: f64) -> f64 {
        if amount_in <= 0.0 || self.reserve_in <= 0.0 || self.reserve_out <= 0.0 {
            return 0.0;
        }
        let fee_factor   = 1.0 - self.fee_bps / 10_000.0;
        let effective_in = amount_in * fee_factor;
        (self.reserve_out * effective_in) / (self.reserve_in + effective_in)
    }

    // ── StableSwap output (Curve convention) ────────────────────────────

    /// Fee is applied to the OUTPUT (dy_raw - fee), matching Curve's specification.
    /// Input enters the invariant unmodified; the solved dy_raw has fee deducted.
    ///
    /// AUDIT FIX #11: was applying fee to input (UniV2 style). Curve applies fee
    /// to output. Corrected to: dy_output = dy_raw - dy_raw * fee_rate.
    /// AUDIT FIX #10: uses relative convergence in D and Y loops.
    pub fn get_output_stable(&self, amount_in: f64) -> f64 {
        if amount_in <= 0.0 || self.reserve_in <= 0.0 || self.reserve_out <= 0.0 {
            return 0.0;
        }

        let n   = self.n_tokens as f64;
        let amp = self.amp;

        // Step 1: compute invariant D from current reserves
        let reserves = [self.reserve_in, self.reserve_out];
        let d = compute_d(amp, &reserves);
        if d <= 0.0 {
            return 0.0;
        }

        // Step 2: new input reserve — NO fee reduction (fee is on output for Curve)
        let new_reserve_in = self.reserve_in + amount_in;

        // Step 3: solve for new output reserve
        let new_reserve_out = compute_y(amp, new_reserve_in, d, n);

        // Step 4: raw output
        let dy_raw = self.reserve_out - new_reserve_out - 1.0;
        if dy_raw <= 0.0 {
            return 0.0;
        }

        // Step 5: deduct fee from OUTPUT (Curve convention — audit fix #11)
        let fee = dy_raw * self.fee_bps / 10_000.0;
        let output = dy_raw - fee;

        if output <= 0.0 { 0.0 } else { output }
    }
}

/// Compute the StableSwap invariant D via Newton-Raphson.
/// AUDIT FIX #10 (F-13): uses RELATIVE convergence (d - prev).abs() / d <= 1e-8
/// instead of absolute (d - prev).abs() <= 1.0, which breaks for large pools.
fn compute_d(amp: f64, reserves: &[f64]) -> f64 {
    let n   = reserves.len() as f64;
    let ann = amp * n.powi(reserves.len() as i32);
    let sum: f64 = reserves.iter().sum();

    if sum == 0.0 {
        return 0.0;
    }

    let mut d = sum;

    for _ in 0..256 {
        let mut d_prod = d;
        for &r in reserves {
            d_prod = d_prod * d / (r * n);
        }

        let d_prev = d;
        d = (ann * sum + n * d_prod) * d
            / ((ann - 1.0) * d + (n + 1.0) * d_prod);

        // RELATIVE convergence — correct for pools of any size (audit fix #10)
        if d > 0.0 && (d - d_prev).abs() / d <= 1e-8 {
            break;
        }
    }

    d
}

/// Solve for the new output reserve y given new input reserve and invariant D.
/// AUDIT FIX #10 (F-13): uses RELATIVE convergence.
fn compute_y(amp: f64, new_reserve_in: f64, d: f64, n: f64) -> f64 {
    let n_int = n as usize;
    let ann   = amp * n.powi(n_int as i32);

    let b = new_reserve_in + d / ann;
    let c = d.powi(n_int as i32 + 1) / (n.powi(n_int as i32) * new_reserve_in * ann);

    let mut y = d;

    for _ in 0..256 {
        let y_prev = y;
        y = (y * y + c) / (2.0 * y + b - d);

        // RELATIVE convergence — correct for pools of any size (audit fix #10)
        if y > 0.0 && (y - y_prev).abs() / y <= 1e-8 {
            break;
        }
    }

    y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(r_in: f64, r_out: f64, fee_bps: f64) -> AmmPool {
        AmmPool { reserve_in: r_in, reserve_out: r_out, fee_bps, amp: 0.0, n_tokens: 2 }
    }

    #[test]
    fn constant_product_zero_fee() {
        let p = pool(1000.0, 1000.0, 0.0);
        let out = p.get_output(100.0);
        let expected = 1000.0 * 100.0 / (1000.0 + 100.0);
        assert!((out - expected).abs() < 1e-9, "got {out}, expected {expected}");
    }

    #[test]
    fn constant_product_with_fee() {
        let p = pool(1_000_000.0, 1_000_000.0, 30.0);
        let out = p.get_output(1000.0);
        assert!(out > 996.0 && out < 997.0, "unexpected output {out}");
    }

    #[test]
    fn zero_input_returns_zero() {
        let p = pool(1000.0, 1000.0, 30.0);
        assert_eq!(p.get_output(0.0), 0.0);
        assert_eq!(p.get_output(-1.0), 0.0);
    }

    #[test]
    fn stableswap_near_parity() {
        let p = AmmPool {
            reserve_in:  1_000_000.0,
            reserve_out: 1_000_000.0,
            fee_bps: 4.0,
            amp:     100.0,
            n_tokens: 2,
        };
        let out = p.get_output_stable(1_000.0);
        assert!(out > 999.0 && out < 1000.0, "stable pool output {out} out of range");
    }

    /// Verify fee-on-output: stable output must be slightly less than no-fee output.
    #[test]
    fn stableswap_fee_reduces_output() {
        let no_fee = AmmPool {
            reserve_in: 1_000_000.0, reserve_out: 1_000_000.0,
            fee_bps: 0.0, amp: 100.0, n_tokens: 2,
        };
        let with_fee = AmmPool {
            reserve_in: 1_000_000.0, reserve_out: 1_000_000.0,
            fee_bps: 4.0, amp: 100.0, n_tokens: 2,
        };
        let out_nofee  = no_fee.get_output_stable(10_000.0);
        let out_fee    = with_fee.get_output_stable(10_000.0);
        assert!(out_fee < out_nofee, "fee should reduce output");
        // Fee should be approximately 0.04% of output
        let fee_fraction = (out_nofee - out_fee) / out_nofee;
        assert!(
            fee_fraction > 0.0003 && fee_fraction < 0.0005,
            "fee fraction {fee_fraction} outside expected range"
        );
    }

    /// Verify relative convergence works on a large pool ($500M reserves).
    #[test]
    fn stableswap_large_pool_convergence() {
        let p = AmmPool {
            reserve_in:  500_000_000_000_000.0, // $500M in 6-decimal USDC units
            reserve_out: 500_000_000_000_000.0,
            fee_bps: 4.0,
            amp:     200.0,
            n_tokens: 2,
        };
        let out = p.get_output_stable(1_000_000_000.0); // $1M swap
        assert!(out > 999_000_000.0 && out < 1_000_000_000.0,
            "large pool output {out} out of range");
    }
}
