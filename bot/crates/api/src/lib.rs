// crates/api/src/lib.rs
//
// REST API: status, strategy enable/disable, manual kill switch.
// Prometheus: /metrics endpoint.
// Telegram: alert on circuit breaker, zero HYPE price, sim divergence.

pub mod metrics;
pub mod rest;
pub mod telegram;

use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use prometheus::{Encoder, TextEncoder};
use serde::{Deserialize, Serialize};
use skua_core::{BotState, SkuaConfig};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tower_http::cors::CorsLayer;

/// Shared API state
#[derive(Clone)]
pub struct ApiState {
    pub bot_state: Arc<BotState>,
    pub api_key:   String,
}

/// Build the Axum router.
pub fn build_router(api_state: ApiState) -> Router {
    Router::new()
        .route("/health",     get(health_handler))
        .route("/status",     get(status_handler))
        .route("/metrics",    get(metrics_handler))
        .route("/kill",       post(kill_handler))
        .route("/resume",     post(resume_handler))
        .route("/strategy",   post(strategy_toggle_handler))
        .layer(CorsLayer::permissive())
        .with_state(api_state)
}

/// Start the API server.
pub async fn serve(state: ApiState, port: u16) -> Result<()> {
    let router = build_router(state);
    let addr   = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(addr, "API server listening");
    axum::serve(listener, router).await?;
    Ok(())
}

// ── Handlers ─────────────────────────────────────────────────────────────────

async fn health_handler() -> StatusCode {
    StatusCode::OK
}

#[derive(Serialize)]
struct StatusResponse {
    block:              u64,
    base_fee:           u64,
    hype_price_usd:     f64,
    kill_switch:        bool,
    consecutive_reverts: u32,
    s1_enabled:         bool,
    s2_enabled:         bool,
    s3_enabled:         bool,
    s4_enabled:         bool,
}

async fn status_handler(State(s): State<ApiState>) -> Json<StatusResponse> {
    let bs = &s.bot_state;
    Json(StatusResponse {
        block:               bs.current_block.load(Ordering::Relaxed),
        base_fee:            bs.current_base_fee.load(Ordering::Relaxed),
        hype_price_usd:      bs.hype_price_f64(),
        kill_switch:         bs.kill_switch.load(Ordering::Relaxed),
        consecutive_reverts: bs.consecutive_reverts.load(Ordering::Relaxed),
        s1_enabled:          bs.s1_enabled.load(Ordering::Relaxed),
        s2_enabled:          bs.s2_enabled.load(Ordering::Relaxed),
        s3_enabled:          bs.s3_enabled.load(Ordering::Relaxed),
        s4_enabled:          bs.s4_enabled.load(Ordering::Relaxed),
    })
}

async fn metrics_handler() -> (StatusCode, String) {
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    if encoder.encode(&metric_families, &mut buffer).is_ok() {
        (StatusCode::OK, String::from_utf8_lossy(&buffer).to_string())
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Failed to encode metrics".to_string())
    }
}

#[derive(Deserialize)]
struct ApiKeyBody {
    api_key: String,
}

async fn kill_handler(
    State(s): State<ApiState>,
    Json(body): Json<ApiKeyBody>,
) -> StatusCode {
    if body.api_key != s.api_key {
        return StatusCode::UNAUTHORIZED;
    }
    s.bot_state.halt("Manual kill via API");
    StatusCode::OK
}

async fn resume_handler(
    State(s): State<ApiState>,
    Json(body): Json<ApiKeyBody>,
) -> StatusCode {
    if body.api_key != s.api_key {
        return StatusCode::UNAUTHORIZED;
    }
    s.bot_state.kill_switch.store(false, Ordering::SeqCst);
    s.bot_state.consecutive_reverts.store(0, Ordering::SeqCst);
    tracing::info!("Kill switch cleared via API");
    StatusCode::OK
}

#[derive(Deserialize)]
struct StrategyToggle {
    api_key:  String,
    strategy: String, // "s1" | "s2" | "s3" | "s4"
    enabled:  bool,
}

async fn strategy_toggle_handler(
    State(s): State<ApiState>,
    Json(body): Json<StrategyToggle>,
) -> StatusCode {
    if body.api_key != s.api_key {
        return StatusCode::UNAUTHORIZED;
    }
    match body.strategy.as_str() {
        "s1" => s.bot_state.s1_enabled.store(body.enabled, Ordering::SeqCst),
        "s2" => s.bot_state.s2_enabled.store(body.enabled, Ordering::SeqCst),
        "s3" => s.bot_state.s3_enabled.store(body.enabled, Ordering::SeqCst),
        "s4" => s.bot_state.s4_enabled.store(body.enabled, Ordering::SeqCst),
        _    => return StatusCode::BAD_REQUEST,
    }
    tracing::info!(strategy = %body.strategy, enabled = body.enabled, "Strategy toggle");
    StatusCode::OK
}
