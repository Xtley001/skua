// crates/hypercore/src/orderbook.rs
//
// HyperCore order book feed via WebSocket.
// Used by S1 signal detector: best bid / ask, and depth for slippage estimation.
// Updates BotState.orderbooks on every push — zero-allocation for the hot path.

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use skua_core::{BotState, state::OrderBook};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;

const HYPERCORE_WS_URL: &str = "wss://api.hyperliquid.xyz/ws";

// ── Wire format (HyperLiquid public WS spec) ─────────────────────────────────

#[derive(Debug, Deserialize)]
struct WsEnvelope {
    channel: String,
    data:    serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct L2BookData {
    coin:   String,
    levels: Vec<Vec<L2Level>>, // levels[0] = bids, levels[1] = asks
    time:   u64,
}

#[derive(Debug, Deserialize)]
struct L2Level {
    px: String,  // price as string
    sz: String,  // size as string
    n:  u32,     // number of orders
}

fn parse_f64(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}

/// Subscribe to the L2 book for `coin` and keep BotState.orderbooks updated.
///
/// Reconnects on disconnection using the outer supervisor loop pattern.
/// Returns Err when the WS stream terminates (caller reconnects).
pub async fn subscribe_l2_book(coin: &str, state: Arc<BotState>) -> Result<()> {
    let url = Url::parse(HYPERCORE_WS_URL).context("Invalid HyperCore WS URL")?;
    let (mut ws, _) = connect_async(&url)
        .await
        .context("HyperCore WS connection failed")?;

    // Subscribe to l2Book channel
    let sub_msg = serde_json::json!({
        "method": "subscribe",
        "subscription": { "type": "l2Book", "coin": coin }
    });
    ws.send(Message::Text(sub_msg.to_string()))
        .await
        .context("Failed to send l2Book subscription")?;

    tracing::info!(coin, "L2 book subscription active");

    while let Some(msg) = ws.next().await {
        let msg = msg.context("HyperCore WS stream error")?;

        match msg {
            Message::Text(text) => {
                if let Err(e) = handle_l2_message(&text, &state) {
                    tracing::warn!(coin, error = %e, "Failed to parse L2 book message");
                }
            }
            Message::Ping(data) => {
                ws.send(Message::Pong(data)).await.ok();
            }
            Message::Close(_) => {
                return Err(anyhow::anyhow!("HyperCore WS closed for coin={coin}"));
            }
            _ => {}
        }
    }

    Err(anyhow::anyhow!("HyperCore WS stream ended for coin={coin}"))
}

fn handle_l2_message(text: &str, state: &Arc<BotState>) -> Result<()> {
    let env: WsEnvelope = serde_json::from_str(text)
        .context("Failed to deserialize WS envelope")?;

    if env.channel != "l2Book" {
        return Ok(()); // not our channel
    }

    let book_data: L2BookData = serde_json::from_value(env.data)
        .context("Failed to deserialize l2Book data")?;

    let bids: Vec<(f64, f64)> = book_data.levels.get(0)
        .map(|levels| levels.iter().map(|l| (parse_f64(&l.px), parse_f64(&l.sz))).collect())
        .unwrap_or_default();

    let asks: Vec<(f64, f64)> = book_data.levels.get(1)
        .map(|levels| levels.iter().map(|l| (parse_f64(&l.px), parse_f64(&l.sz))).collect())
        .unwrap_or_default();

    let best_bid = bids.first().map(|(p, _)| *p).unwrap_or(0.0);
    let best_ask = asks.first().map(|(p, _)| *p).unwrap_or(0.0);

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let ob = OrderBook {
        coin:            book_data.coin.clone(),
        best_bid,
        best_ask,
        bid_depth:       bids,
        ask_depth:       asks,
        last_updated_ms: now_ms,
    };

    state.orderbooks.write().insert(book_data.coin, ob);
    Ok(())
}

/// Estimate the average fill price for selling `size` tokens into the bid side.
/// Used in S1 profit model to account for order-book slippage.
pub fn estimate_core_sell_slippage(
    order_book: &OrderBook,
    size:        f64,
) -> f64 {
    if order_book.bid_depth.is_empty() || size <= 0.0 {
        return 1.0; // 100% slippage → signal unprofitable
    }

    let mut remaining = size;
    let mut total_value = 0.0;

    for (price, level_size) in &order_book.bid_depth {
        let fill = remaining.min(*level_size);
        total_value += fill * price;
        remaining -= fill;
        if remaining <= 0.0 {
            break;
        }
    }

    if remaining > 0.0 {
        // Order book too shallow to fill the entire size
        return 1.0;
    }

    let vwap = total_value / size;
    let best_bid = order_book.best_bid;
    if best_bid <= 0.0 {
        return 1.0;
    }

    // Return fractional slippage: (best_bid - vwap) / best_bid
    ((best_bid - vwap) / best_bid).max(0.0)
}
