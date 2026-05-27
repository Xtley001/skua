// crates/executor/src/wallet_pool.rs
//
// SKUA Law 5: Eight nonces per address, no exceptions.
// Multi-wallet architecture is mandatory. Minimum 5 wallets, recommended 8–10.
//
// Nonce tracking: local AtomicU64 — never fetched from chain in the hot path.
// Initial nonces are fetched once at startup. reset_nonce() is for recovery only.

use anyhow::{Context, Result};
use alloy::network::EthereumWallet;
use alloy::signers::local::PrivateKeySigner;
use alloy::primitives::Address;
use alloy::providers::Provider;
use skua_chain::HttpProvider;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A wallet slot in the pool.
pub struct WalletSlot {
    pub signer:       PrivateKeySigner,
    pub wallet:       EthereumWallet,
    pub address:      Address,
    pub nonce:        AtomicU64,
    pub last_used_ms: AtomicU64,
    pub in_use:       AtomicBool,
}

impl WalletSlot {
    fn new(signer: PrivateKeySigner, initial_nonce: u64) -> Self {
        let address = signer.address();
        let wallet  = EthereumWallet::from(signer.clone());
        Self {
            signer,
            wallet,
            address,
            nonce:        AtomicU64::new(initial_nonce),
            last_used_ms: AtomicU64::new(0),
            in_use:       AtomicBool::new(false),
        }
    }
}

/// Thread-safe pool of hot wallets.
///
/// Each wallet handles its own nonce independently — no cross-wallet coordination
/// required during execution. The LRU acquire policy spreads load evenly.
pub struct WalletPool {
    slots: Vec<WalletSlot>,
}

impl WalletPool {
    /// Build the pool from private key signers and pre-fetched nonces.
    /// `initial_nonces` must be fetched from chain at startup —
    /// one `eth_getTransactionCount` per address.
    pub fn new(signers: Vec<PrivateKeySigner>, initial_nonces: Vec<u64>) -> Self {
        assert_eq!(
            signers.len(),
            initial_nonces.len(),
            "WalletPool: signer count and nonce count must match"
        );
        assert!(signers.len() >= 5, "WalletPool: minimum 5 wallets required");

        let slots = signers
            .into_iter()
            .zip(initial_nonces)
            .map(|(s, n)| WalletSlot::new(s, n))
            .collect();

        Self { slots }
    }

    /// Acquire the least-recently-used wallet that is not currently in use.
    /// Returns the wallet index, or `None` if all wallets are busy.
    ///
    /// Uses `compare_exchange` to atomically claim the wallet — race-free.
    pub fn acquire(&self) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.in_use
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            })
            .min_by_key(|(i, _)| self.slots[*i].last_used_ms.load(Ordering::Relaxed))
            .map(|(i, _)| i)
    }

    /// Get the next nonce for wallet `idx` and increment the counter atomically.
    ///
    /// `fetch_add` returns the PREVIOUS value — which is the correct nonce for this tx.
    /// Never fetches from chain in the hot path.
    pub fn next_nonce(&self, idx: usize) -> u64 {
        self.slots[idx].nonce.fetch_add(1, Ordering::SeqCst)
    }

    /// Release a wallet back to the pool after a transaction is complete.
    pub fn release(&self, idx: usize) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.slots[idx].last_used_ms.store(now_ms, Ordering::Relaxed);
        self.slots[idx].in_use.store(false, Ordering::Release);
    }

    /// Get the wallet's EthereumWallet for transaction signing.
    pub fn wallet(&self, idx: usize) -> &EthereumWallet {
        &self.slots[idx].wallet
    }

    /// Get the wallet's address.
    pub fn address(&self, idx: usize) -> Address {
        self.slots[idx].address
    }

    /// Recovery: re-fetch nonce from chain after detecting a stuck transaction.
    /// Only called outside the hot path.
    pub async fn reset_nonce(&self, idx: usize, http: &HttpProvider) -> Result<()> {
        let addr = self.slots[idx].address;
        let on_chain = http
            .get_transaction_count(addr)
            .await
            .with_context(|| format!("Failed to fetch nonce for wallet {addr}"))?;
        self.slots[idx].nonce.store(on_chain, Ordering::SeqCst);
        tracing::info!(wallet = %addr, nonce = on_chain, "Nonce reset from chain");
        Ok(())
    }

    /// Fetch all initial nonces from chain at startup.
    /// Call this once before constructing WalletPool.
    pub async fn fetch_initial_nonces(
        signers: &[PrivateKeySigner],
        http:    &HttpProvider,
    ) -> Result<Vec<u64>> {
        let mut nonces = Vec::with_capacity(signers.len());
        for s in signers {
            let n = http
                .get_transaction_count(s.address())
                .await
                .with_context(|| format!("Failed to fetch nonce for {}", s.address()))?;
            nonces.push(n);
        }
        Ok(nonces)
    }

    /// Number of wallets in the pool.
    pub fn len(&self) -> usize {
        self.slots.len()
    }
}
