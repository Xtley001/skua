pub mod amm;
pub mod eth_call;
pub mod profit;
pub mod sizing;

pub use amm::AmmPool;
pub use eth_call::simulate_and_validate;
pub use profit::{
    net_profit_two_leg,
    net_profit_three_leg,
    net_profit_stable_depeg,
    effective_min_profit_usd,
};
pub use sizing::{optimal_borrow_size, stage1_upper_bound};
