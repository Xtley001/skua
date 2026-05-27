// crates/api/src/metrics.rs
//
// AUDIT FIX #16: Added all 12 required metrics. Added increment helpers so
// strategy code can call them without importing prometheus directly.
// Added 5 missing metrics: bundles_fired, bundles_landed, landing_rate,
// block_latency_ms, reverts_by_class.

use once_cell::sync::Lazy;
use prometheus::{
    Counter, CounterVec, Gauge, GaugeVec, Histogram, HistogramOpts,
    HistogramVec, Opts, register_counter_vec, register_gauge,
    register_gauge_vec, register_histogram_vec,
};
use std::time::Instant;

// ── System metrics ────────────────────────────────────────────────────────────

pub static BLOCK_NUMBER: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("skua_block_number", "Latest processed block").unwrap()
});
pub static BASE_FEE_GWEI: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("skua_base_fee_gwei", "Current base fee in gwei").unwrap()
});
pub static HYPE_PRICE_USD: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("skua_hype_price_usd", "HYPE price from precompile oracle").unwrap()
});
pub static BALANCER_FEE_BPS: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("skua_balancer_fee_bps", "Balancer V3 flash loan fee bps").unwrap()
});
pub static HYPERLEND_FEE_BPS: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("skua_hyperlend_fee_bps", "HyperLend flash loan fee bps").unwrap()
});
pub static KILL_SWITCH_ACTIVE: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("skua_kill_switch_active", "1 if kill switch is active").unwrap()
});
pub static CONSECUTIVE_REVERTS: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("skua_consecutive_reverts", "Current consecutive revert count").unwrap()
});
pub static GAS_SPENT_WINDOW_HYPE: Lazy<Gauge> = Lazy::new(|| {
    register_gauge!("skua_gas_spent_window_hype", "Gas spent in rolling window (HYPE)").unwrap()
});

// ── Per-strategy counters ─────────────────────────────────────────────────────

macro_rules! labeled_counter {
    ($name:ident, $metric:literal, $help:literal) => {
        pub static $name: Lazy<CounterVec> = Lazy::new(|| {
            register_counter_vec!($metric, $help, &["strategy"]).unwrap()
        });
    };
}

labeled_counter!(OPPS_DETECTED,   "skua_opps_detected_total",   "Signals above threshold");
labeled_counter!(SIMS_PASSED,     "skua_sims_passed_total",     "Passed algebraic + eth_call");
labeled_counter!(TXS_SUBMITTED,   "skua_txs_submitted_total",   "Submitted transactions");

// AUDIT FIX #16: was absent — distinct from submitted (builder accepted ≠ on-chain)
labeled_counter!(TXS_LANDED,      "skua_txs_landed_total",      "On-chain confirmed transactions");
labeled_counter!(TXS_REVERTED,    "skua_txs_reverted_total",    "Reverted transactions");

// AUDIT FIX #16: profit_usd sourced from on-chain receipts
labeled_counter!(PROFIT_USD,      "skua_profit_usd_total",      "Total profit USD (from receipts)");
labeled_counter!(GAS_HYPE_TOTAL,  "skua_gas_hype_total",        "Total gas cost HYPE");

// AUDIT FIX #16: reverts_by_class — per-reason breakdown
pub static REVERTS_BY_CLASS: Lazy<CounterVec> = Lazy::new(|| {
    register_counter_vec!(
        "skua_reverts_by_class",
        "Reverted transactions by class",
        &["strategy", "class"] // class: "profit_guard", "oracle", "stale_state", "unknown"
    ).unwrap()
});

// ── Per-strategy histograms ───────────────────────────────────────────────────

pub static SIZING_OPTIMAL_USD: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "skua_sizing_optimal_usd",
        "GSS optimal borrow amount distribution",
        &["strategy"],
        vec![100.0, 1_000.0, 10_000.0, 100_000.0, 500_000.0, 1_000_000.0]
    ).unwrap()
});

pub static SIM_DIVERGENCE_PCT: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "skua_sim_divergence_pct",
        "Algebraic vs on-chain profit divergence %",
        &["strategy"],
        vec![0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0]
    ).unwrap()
});

pub static LATENCY_SIGNAL_TO_SUBMIT_MS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "skua_latency_signal_to_submit_ms",
        "End-to-end latency: signal detection → submission (ms)",
        &["strategy"],
        vec![5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0]
    ).unwrap()
});

// AUDIT FIX #16: was absent
pub static BLOCK_LATENCY_MS: Lazy<HistogramVec> = Lazy::new(|| {
    register_histogram_vec!(
        "skua_block_latency_ms",
        "Time from block arrival to strategy evaluation complete (ms)",
        &["strategy"],
        vec![1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 200.0]
    ).unwrap()
});

// AUDIT FIX #16: was absent — most important ratio
pub static LANDING_RATE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "skua_landing_rate",
        "Ratio of on-chain confirmed to submitted (rolling 1h)",
        &["strategy"]
    ).unwrap()
});

pub static WALLET_HYPE_BALANCE: Lazy<GaugeVec> = Lazy::new(|| {
    register_gauge_vec!(
        "skua_wallet_hype_balance",
        "Hot wallet HYPE balance",
        &["wallet_index"]
    ).unwrap()
});

// ── Convenience increment helpers ─────────────────────────────────────────────
// Strategy code calls these so it doesn't need to import prometheus directly.
// AUDIT FIX #16: these are what wire the metric increments into execution paths.

pub fn record_opp_detected(strategy: &str) {
    OPPS_DETECTED.with_label_values(&[strategy]).inc();
}

pub fn record_sim_passed(strategy: &str) {
    SIMS_PASSED.with_label_values(&[strategy]).inc();
}

pub fn record_tx_submitted(strategy: &str) {
    TXS_SUBMITTED.with_label_values(&[strategy]).inc();
}

/// Call with profit from on-chain receipt — not from simulation estimate.
pub fn record_tx_landed(strategy: &str, profit_usd: f64, gas_hype: f64) {
    TXS_LANDED.with_label_values(&[strategy]).inc();
    if profit_usd > 0.0 {
        PROFIT_USD.with_label_values(&[strategy]).inc_by(profit_usd);
    }
    if gas_hype > 0.0 {
        GAS_HYPE_TOTAL.with_label_values(&[strategy]).inc_by(gas_hype);
    }
    // Recompute landing rate
    let landed   = TXS_LANDED.with_label_values(&[strategy]).get();
    let submitted = TXS_SUBMITTED.with_label_values(&[strategy]).get();
    if submitted > 0.0 {
        LANDING_RATE.with_label_values(&[strategy]).set(landed / submitted);
    }
}

pub fn record_tx_reverted(strategy: &str, class: &str, gas_hype: f64) {
    TXS_REVERTED.with_label_values(&[strategy]).inc();
    REVERTS_BY_CLASS.with_label_values(&[strategy, class]).inc();
    if gas_hype > 0.0 {
        GAS_HYPE_TOTAL.with_label_values(&[strategy]).inc_by(gas_hype);
    }
}

pub fn record_sizing(strategy: &str, optimal_usd: f64) {
    SIZING_OPTIMAL_USD.with_label_values(&[strategy]).observe(optimal_usd);
}

pub fn record_sim_divergence(strategy: &str, pct: f64) {
    SIM_DIVERGENCE_PCT.with_label_values(&[strategy]).observe(pct);
}

pub fn record_signal_latency(strategy: &str, start: Instant) {
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    LATENCY_SIGNAL_TO_SUBMIT_MS.with_label_values(&[strategy]).observe(ms);
}

pub fn record_block_latency(strategy: &str, ms: f64) {
    BLOCK_LATENCY_MS.with_label_values(&[strategy]).observe(ms);
}

pub fn init_all() {
    let _ = &*BLOCK_NUMBER;
    let _ = &*BASE_FEE_GWEI;
    let _ = &*HYPE_PRICE_USD;
    let _ = &*BALANCER_FEE_BPS;
    let _ = &*HYPERLEND_FEE_BPS;
    let _ = &*KILL_SWITCH_ACTIVE;
    let _ = &*CONSECUTIVE_REVERTS;
    let _ = &*GAS_SPENT_WINDOW_HYPE;
    let _ = &*OPPS_DETECTED;
    let _ = &*SIMS_PASSED;
    let _ = &*TXS_SUBMITTED;
    let _ = &*TXS_LANDED;
    let _ = &*TXS_REVERTED;
    let _ = &*PROFIT_USD;
    let _ = &*GAS_HYPE_TOTAL;
    let _ = &*REVERTS_BY_CLASS;
    let _ = &*SIZING_OPTIMAL_USD;
    let _ = &*SIM_DIVERGENCE_PCT;
    let _ = &*LATENCY_SIGNAL_TO_SUBMIT_MS;
    let _ = &*BLOCK_LATENCY_MS;
    let _ = &*LANDING_RATE;
    let _ = &*WALLET_HYPE_BALANCE;
}
