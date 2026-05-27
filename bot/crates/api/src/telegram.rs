// crates/api/src/telegram.rs
//
// AUDIT FIX #17: All 6 required alert methods fully implemented and documented.
// Callers must wire these into the execution paths (see main.rs and submit.rs).

use anyhow::Result;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct TelegramAlerter {
    bot_token: String,
    chat_id:   String,
    client:    reqwest::Client,
}

impl TelegramAlerter {
    pub fn new(bot_token: String, chat_id: String) -> Self {
        Self { bot_token, chat_id, client: reqwest::Client::new() }
    }

    pub async fn send(&self, message: &str) -> Result<()> {
        let url     = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let payload = serde_json::json!({
            "chat_id":    self.chat_id,
            "text":       message,
            "parse_mode": "Markdown"
        });
        self.client.post(&url).json(&payload).send().await?;
        Ok(())
    }

    // ── AUDIT FIX #17: All 6 required alerts ─────────────────────────────

    /// Alert 1: Kill switch activated (circuit breaker or manual).
    /// Wire into: BotState::halt(), submit_sync revert threshold.
    pub async fn alert_kill_switch(&self, reason: &str) {
        let ts  = unix_ts();
        let msg = format!("🚨 *SKUA KILL SWITCH*\nReason: `{reason}`\nTime: `{ts}`");
        if let Err(e) = self.send(&msg).await {
            tracing::error!("Telegram send failed: {e}");
        }
    }

    /// Alert 2: Hot wallet balance below threshold.
    /// Wire into: wallet balance check in submit.rs after each tx.
    /// Threshold: < 0.05 HYPE on HyperEVM.
    pub async fn alert_low_balance(&self, wallet: &str, balance_hype: f64) {
        let msg = format!(
            "⚠️ *Low HYPE Balance*\nWallet: `{wallet}`\nBalance: `{balance_hype:.4} HYPE`\n\
             Action: top up immediately"
        );
        if let Err(e) = self.send(&msg).await { tracing::error!("Telegram send failed: {e}"); }
    }

    /// Alert 3: HYPE price precompile returned 0.
    /// Wire into: block loop HYPE price refresh failure.
    pub async fn alert_zero_price(&self) {
        let ts  = unix_ts();
        let msg = format!(
            "🔴 *CRITICAL: HYPE price precompile returned 0*\n\
             All submissions blocked.\nTime: `{ts}`"
        );
        if let Err(e) = self.send(&msg).await { tracing::error!("Telegram send failed: {e}"); }
    }

    /// Alert 4: Simulation divergence > 0.5% — auto-pause triggered.
    /// Wire into: eth_call.rs divergence check.
    pub async fn alert_sim_divergence(&self, strategy: &str, algebraic: f64, onchain: f64) {
        let pct = ((algebraic - onchain).abs() / algebraic.abs().max(1e-9)) * 100.0;
        let msg = format!(
            "⚠️ *Sim Divergence Auto-Pause: {strategy}*\n\
             Algebraic: `${algebraic:.2}` | On-chain: `${onchain:.2}`\n\
             Divergence: `{pct:.2}%`\nBot paused — investigate pool state"
        );
        if let Err(e) = self.send(&msg).await { tracing::error!("Telegram send failed: {e}"); }
    }

    /// Alert 5: Landing rate < 20% for 30+ minutes.
    /// Wire into: landing-rate monitor in metrics sync loop.
    pub async fn alert_low_landing_rate(&self, strategy: &str, rate_pct: f64, window_min: u64) {
        let msg = format!(
            "⚠️ *Low Landing Rate: {strategy}*\n\
             Rate: `{rate_pct:.1}%` over last `{window_min}` min\n\
             Threshold: 20% — check builder connectivity"
        );
        if let Err(e) = self.send(&msg).await { tracing::error!("Telegram send failed: {e}"); }
    }

    /// Alert 6: No blocks processed for > 30 seconds.
    /// Wire into: block loop supervisor (checks last_block_time).
    pub async fn alert_no_blocks(&self, seconds_since_last: u64) {
        let msg = format!(
            "🔴 *No Blocks Processed*\n\
             Last block: `{seconds_since_last}s` ago\n\
             Action: check RPC connectivity and node health"
        );
        if let Err(e) = self.send(&msg).await { tracing::error!("Telegram send failed: {e}"); }
    }

    /// Profit notification (non-critical, informational).
    pub async fn alert_profit(&self, strategy: &str, profit_usd: f64, tx_hash: &str) {
        let msg = format!(
            "✅ *{strategy} Profit*\n`${profit_usd:.2}` | tx: `{tx_hash}`"
        );
        if let Err(e) = self.send(&msg).await { tracing::error!("Telegram send failed: {e}"); }
    }
}

fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
