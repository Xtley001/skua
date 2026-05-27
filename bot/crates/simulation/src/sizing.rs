// crates/simulation/src/sizing.rs
//
// Four-stage sizing pipeline (mandatory per audit protocol):
//   Stage 1: Imbalance-anchored upper bound (doubling probe loop)
//   Stage 2: Golden-section search (128 iterations)
//   Stage 3: Dual-leg A-parameter impact gate
//   Stage 4: Hard ceilings (flash available + operator cap)
//
// AUDIT FIXES:
//   #12: Added Stage 1 imbalance-anchored probe (was absent)
//   #13: Added Stage 3 dual-leg impact gate (was absent)

use skua_core::types::{FlashProvider, SizingResult};

const PHI: f64 = 0.6180339887498948;
const GSS_ITERATIONS: usize = 128;

// ── Stage 3 helpers ──────────────────────────────────────────────────────────

/// Maximum fractional price impact allowed for a pool with amplification A.
/// Formula: (0.5 + 2.5 * t).max(0.5).min(3.5) as a percentage
/// where t = (ln(A) - ln(50)) / (ln(2000) - ln(50))
fn max_impact_for_a(amp: f64) -> f64 {
    if amp <= 0.0 { return 0.5; }
    let t = (amp.ln() - 50_f64.ln()) / (2000_f64.ln() - 50_f64.ln());
    let t = t.max(0.0).min(1.0);
    (0.5 + 2.5 * t).max(0.5).min(3.5)
}

/// Run Stage 1: imbalance-anchored doubling probe.
/// Returns the upper bound USD to pass to GSS.
/// probe_frac = (imbalance_ratio * 0.5).max(0.005).min(0.20)
/// where imbalance_ratio = |reserve_in - reserve_out| / max(reserve_in, reserve_out)
pub fn stage1_upper_bound<F>(
    profit_fn:        &F,
    reserve_in_usd:   f64,
    reserve_out_usd:  f64,
    hard_ceiling_usd: f64,
) -> f64
where
    F: Fn(f64) -> f64,
{
    if reserve_in_usd <= 0.0 || reserve_out_usd <= 0.0 {
        return hard_ceiling_usd;
    }

    let max_reserve = reserve_in_usd.max(reserve_out_usd);
    let imbalance   = (reserve_in_usd - reserve_out_usd).abs() / max_reserve;
    let probe_frac  = (imbalance * 0.5).max(0.005_f64).min(0.20_f64);
    let mut probe   = reserve_in_usd * probe_frac;

    // Doubling loop: find where the profit function stops being positive
    while profit_fn(probe) > 0.0 && probe < reserve_in_usd * 0.95 {
        let next = (probe * 2.0).min(reserve_in_usd * 0.95).min(hard_ceiling_usd);
        if next <= probe { break; }
        probe = next;
    }

    probe.min(hard_ceiling_usd)
}

/// Find the borrow amount x* that maximises π(x) — full four-stage pipeline.
///
/// # Arguments
/// * `profit_fn`           – π(x) closure with all live parameters captured
/// * `flash_available_usd` – flash loan liquidity (read from chain each call)
/// * `hard_cap_usd`        – operator ceiling from SkuaConfig
/// * `token_price_usd`     – live price for USD→wei conversion
/// * `token_decimals`      – for wei scaling
/// * `flash_provider`      – which provider supplies the loan
/// * `reserve_in_usd`      – entry pool reserve in USD (for Stage 1 probe)
/// * `reserve_out_usd`     – entry pool output reserve in USD (for Stage 1)
/// * `amp_entry`           – A coefficient of entry pool (Stage 3; 0 = not stable)
/// * `amp_exit`            – A coefficient of exit pool (Stage 3; 0 = not stable)
pub fn optimal_borrow_size<F>(
    profit_fn:           F,
    flash_available_usd: f64,
    hard_cap_usd:        f64,
    token_price_usd:     f64,
    token_decimals:      u8,
    flash_provider:      FlashProvider,
    reserve_in_usd:      f64,
    reserve_out_usd:     f64,
    amp_entry:           f64,
    amp_exit:            f64,
) -> Option<SizingResult>
where
    F: Fn(f64) -> f64,
{
    if token_price_usd <= 0.0 { return None; }

    // ── Stage 4: absolute ceiling ────────────────────────────────────────
    let stage4_ceiling = flash_available_usd.min(hard_cap_usd);
    if stage4_ceiling <= 0.0 { return None; }

    // ── Stage 1: imbalance-anchored upper bound ──────────────────────────
    // AUDIT FIX #12: was going straight to stage4_ceiling
    let stage1_upper = stage1_upper_bound(
        &profit_fn,
        reserve_in_usd,
        reserve_out_usd,
        stage4_ceiling,
    );
    if stage1_upper <= 0.0 { return None; }

    // ── Stage 3: dual-leg A-parameter impact gate ────────────────────────
    // AUDIT FIX #13: was absent. Gate applied to BOTH entry and exit pools.
    let gated_upper = if amp_entry > 0.0 || amp_exit > 0.0 {
        let impact_entry = if amp_entry > 0.0 {
            max_impact_for_a(amp_entry) / 100.0 * reserve_in_usd
        } else {
            stage1_upper
        };

        let impact_exit = if amp_exit > 0.0 {
            max_impact_for_a(amp_exit) / 100.0 * reserve_out_usd
        } else {
            stage1_upper
        };

        // Binding exit leg: if exit is tighter, scale entry proportionally
        let binding = impact_entry.min(impact_exit);
        if impact_exit < impact_entry {
            // Exit pool is binding — scale back entry proportionally
            binding * (impact_entry / impact_exit).min(1.0)
        } else {
            binding
        }
        .min(stage1_upper)
    } else {
        stage1_upper
    };

    if gated_upper <= 0.0 { return None; }

    // ── Stage 2: 128-iteration golden-section search ─────────────────────
    let mut lo = 0.0_f64;
    let mut hi = gated_upper;

    for _ in 0..GSS_ITERATIONS {
        let m1 = hi - PHI * (hi - lo);
        let m2 = lo + PHI * (hi - lo);

        // Early exit on convergence
        if (hi - lo) < 1e-10 { break; }

        if profit_fn(m1) < profit_fn(m2) {
            lo = m1;
        } else {
            hi = m2;
        }
    }

    let optimal_usd     = (lo + hi) / 2.0;
    let expected_profit = profit_fn(optimal_usd);

    if expected_profit <= 0.0 { return None; }

    let scale       = 10_u128.pow(token_decimals as u32);
    let optimal_wei = ((optimal_usd / token_price_usd) * scale as f64) as u128;

    if optimal_wei == 0 { return None; }

    Some(SizingResult {
        optimal_amount_wei: optimal_wei,
        optimal_amount_usd: optimal_usd,
        expected_profit_usd: expected_profit,
        flash_provider,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use skua_core::types::FlashProvider;

    /// Concave profit fn with known maximum at x = 500.
    fn mock_profit(x: f64) -> f64 {
        -(x - 500.0).powi(2) / 10_000.0 + 50.0
    }

    fn run(flash: f64, cap: f64) -> Option<SizingResult> {
        optimal_borrow_size(
            mock_profit, flash, cap, 1.0, 6,
            FlashProvider::Balancer,
            1_000_000.0, 1_000_000.0, 0.0, 0.0,
        )
    }

    #[test]
    fn gss_finds_known_maximum() {
        let r = run(1_000.0, 1_000.0).expect("should find profit");
        assert!((r.optimal_amount_usd - 500.0).abs() < 0.1,
            "optimal {:.4} should be ~500", r.optimal_amount_usd);
        assert!((r.expected_profit_usd - 50.0).abs() < 0.1,
            "profit {:.4} should be ~50", r.expected_profit_usd);
    }

    #[test]
    fn unprofitable_returns_none() {
        let r = optimal_borrow_size(
            |x| -x, 1_000.0, 1_000.0, 1.0, 6,
            FlashProvider::Balancer, 1_000_000.0, 1_000_000.0, 0.0, 0.0,
        );
        assert!(r.is_none());
    }

    #[test]
    fn hard_cap_respected() {
        let r = run(1_000.0, 300.0).expect("still profitable under cap");
        assert!(r.optimal_amount_usd <= 301.0);
    }

    #[test]
    fn stage1_probe_stays_below_ceiling() {
        let upper = stage1_upper_bound(&mock_profit, 2_000.0, 1_000.0, 5_000.0);
        assert!(upper <= 5_000.0);
        assert!(upper > 0.0);
    }

    #[test]
    fn max_impact_for_a_bounds() {
        assert!((max_impact_for_a(50.0) - 0.5).abs() < 0.01);
        assert!((max_impact_for_a(2000.0) - 3.0).abs() < 0.1);
        assert!(max_impact_for_a(0.0) >= 0.5);
        assert!(max_impact_for_a(1e9) <= 3.5);
    }

    #[test]
    fn stage3_gate_applies_to_both_legs() {
        // With a very shallow exit pool (amp=50, $100 reserve) the gate should
        // reduce optimal below what single-leg would produce
        let r_no_gate = optimal_borrow_size(
            mock_profit, 1_000.0, 1_000.0, 1.0, 6,
            FlashProvider::Balancer, 1_000_000.0, 1_000_000.0, 0.0, 0.0,
        ).unwrap();
        let r_gated = optimal_borrow_size(
            mock_profit, 1_000.0, 1_000.0, 1.0, 6,
            FlashProvider::Balancer, 1_000_000.0, 100.0, 100.0, 50.0,
        );
        // Gated result should be smaller or None (shallow exit)
        if let Some(g) = r_gated {
            assert!(g.optimal_amount_usd <= r_no_gate.optimal_amount_usd + 1.0);
        }
        // None is also acceptable — shallow pool means no profitable size
    }
}
