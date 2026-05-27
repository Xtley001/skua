// crates/hypercore/src/corewriter.rs
//
// CoreWriter: 0x3333333333333333333333333333333333333333 (immutable constant)
//
// SKUA Law 2: CoreWriter delay is PERMANENT by design (~3–5 seconds).
// CoreWriter is NEVER called within the same atomic transaction as flash loan repay.
// S1 Phase 2 only. S2/S3/S4 never touch CoreWriter.
//
// Price scaling: HyperCore expects prices as uint64 = human_price × 10^8
// Size scaling:  HyperCore expects size as uint64 = human_size × 10^8

use alloy::primitives::{address, Address};
use alloy::sol;

/// CoreWriter address — immutable constant, never stored in a variable slot.
pub const COREWRITER_ADDRESS: Address =
    address!("3333333333333333333333333333333333333333");

/// Time-in-force options for CoreWriter orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Tif {
    /// Alo (post-only / add-liquidity-only)
    Alo = 1,
    /// Gtc (good-till-cancelled)
    Gtc = 2,
    /// Ioc (immediate-or-cancel) — preferred for SKUA Phase 2 sells
    Ioc = 3,
}

sol! {
    #[allow(missing_docs)]
    interface ICoreWriter {
        function placeOrder(
            uint32 marketIndex,
            bool   isBuy,
            uint64 limitPx,
            uint64 sz,
            uint8  tif,
            uint64 cloid
        ) external;

        function spotSend(
            address to,
            uint32  tokenIndex,
            uint64  amount
        ) external;
    }
}

/// Parameters for a CoreWriter placeOrder call.
#[derive(Debug, Clone)]
pub struct PlaceOrderParams {
    pub market_index: u32,
    pub is_buy:       bool,
    /// Human-readable price (e.g. 25.50 USD). Scaled to 10^8 when encoding.
    pub limit_px_usd: f64,
    /// Human-readable size (e.g. 1.5 tokens). Scaled to 10^8 when encoding.
    pub size:         f64,
    pub tif:          Tif,
    /// Client order ID (0 = no ID)
    pub cloid:        u64,
}

impl PlaceOrderParams {
    /// Encode price to CoreWriter's fixed-point format (human_price × 10^8)
    pub fn encode_price(&self) -> u64 {
        (self.limit_px_usd * 1e8) as u64
    }

    /// Encode size to CoreWriter's fixed-point format (human_size × 10^8)
    pub fn encode_size(&self) -> u64 {
        (self.size * 1e8) as u64
    }

    /// Build the calldata bytes for placeOrder.
    pub fn calldata(&self) -> alloy::primitives::Bytes {
        let iface = ICoreWriter::placeOrderCall {
            marketIndex: self.market_index,
            isBuy:       self.is_buy,
            limitPx:     self.encode_price(),
            sz:          self.encode_size(),
            tif:         self.tif as u8,
            cloid:       self.cloid,
        };
        alloy::primitives::Bytes::from(alloy::sol_types::SolCall::abi_encode(&iface))
    }
}

/// Build a Phase 2 IOC sell order for S1.
///
/// `limit_px_usd` should be set at or slightly below HyperCore best bid
/// to guarantee a fill. The IOC TIF cancels any unfilled remainder.
pub fn build_s1_sell_order(
    market_index:  u32,
    size:          f64,
    limit_px_usd:  f64,
    cloid:         u64,
) -> PlaceOrderParams {
    PlaceOrderParams {
        market_index,
        is_buy:       false,
        limit_px_usd,
        size,
        tif:   Tif::Ioc,
        cloid,
    }
}
