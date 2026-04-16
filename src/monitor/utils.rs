use alloy::primitives::{aliases::U256, utils};

pub fn approx_units(value: U256) -> f64 {
    utils::format_ether(value)
        .parse::<f64>()
        .expect("invalid formatted units")
}

pub fn approx_gwei(value: u128) -> f64 {
    value as f64 / 1e9
}
