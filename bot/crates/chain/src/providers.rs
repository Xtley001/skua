// crates/chain/src/providers.rs
//
// HyperEVM has NO IPC socket. WS and HTTP only.
// WebSocket: block/log subscriptions (event-driven, never poll).
// HTTP:      eth_call, nonce fetch, simulation.
//
// Reconnect: exponential backoff with jitter, capped at 30s.

use anyhow::{Context, Result};
use skua_core::SkuaConfig;
use std::time::Duration;
use tokio::time::sleep;
use alloy::providers::{ProviderBuilder, WsConnect};
use alloy::providers::Provider;
use alloy::transports::http::Http;
use alloy::transports::ws::WsTransport;

pub type WsProvider  = alloy::providers::RootProvider<WsTransport>;
pub type HttpProvider = alloy::providers::RootProvider<Http<reqwest::Client>>;

/// Build both providers. Fails fast if either connection cannot be established.
pub async fn build_providers(config: &SkuaConfig) -> Result<(WsProvider, HttpProvider)> {
    let ws = ProviderBuilder::new()
        .on_ws(WsConnect::new(&config.rpc_ws_url))
        .await
        .context("WS provider connection failed")?;

    let http = ProviderBuilder::new()
        .on_http(config.rpc_http_url.parse().context("Invalid HTTP RPC URL")?)
        ;

    Ok((ws, http))
}

/// WebSocket provider with automatic exponential-backoff reconnect.
/// Never returns — loops forever until a successful connection or process exit.
pub async fn ws_with_reconnect(config: &SkuaConfig) -> WsProvider {
    let mut delay = Duration::from_millis(100);
    loop {
        match ProviderBuilder::new()
            .on_ws(WsConnect::new(&config.rpc_ws_url))
            .await
        {
            Ok(p) => {
                tracing::info!(url = %config.rpc_ws_url, "WS provider connected");
                return p;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    retry_in = ?delay,
                    "WS reconnect attempt failed"
                );
                sleep(delay).await;
                // jitter: delay × 2 + rand(0..100ms), capped at 30s
                let jitter = Duration::from_millis(rand::random::<u8>() as u64 % 100);
                delay = (delay * 2 + jitter).min(Duration::from_secs(30));
            }
        }
    }
}

/// Try the primary WS URL; on failure, fall over to `rpc_fallback_url`.
pub async fn ws_with_fallback(config: &SkuaConfig) -> WsProvider {
    match ProviderBuilder::new()
        .on_ws(WsConnect::new(&config.rpc_ws_url))
        .await
    {
        Ok(p) => {
            tracing::info!("Primary WS connected");
            p
        }
        Err(primary_err) => {
            tracing::warn!(
                error = %primary_err,
                "Primary WS failed, switching to fallback"
            );
            let mut fallback_config = config.clone();
            fallback_config.rpc_ws_url = config.rpc_fallback_url.clone();
            ws_with_reconnect(&fallback_config).await
        }
    }
}
