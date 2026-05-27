pub mod pipeline;
pub mod s1_signal;
pub mod s4_signal;

pub use pipeline::{
    layer1_delta_filter, layer2_liquidity_gate, layer3_sizing,
    layer4_profit_check, layer5_eth_call_sim,
};
pub use s1_signal::{S1Detector, S1Signal};
pub use s4_signal::{check_depeg, DepegSignal};
