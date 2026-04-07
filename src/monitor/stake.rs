use alloy::primitives::Address;
use prometheus::GaugeVec;

/// Monitors validator stake on the staking contract and tracks current stake
/// amounts as Prometheus metrics.
pub struct ValidatorStake {
    staking_rpc: String,
    contract: Address,
    stake: GaugeVec,
}

impl ValidatorStake {
    pub fn new(staking_rpc: String, contract: Address) -> Result<Self, prometheus::Error> {
        let stake = prometheus::register_gauge_vec!(
            "validator_stake",
            "Current stake amount per validator on the Safenet staking contract.",
            &["validator"]
        )?;

        Ok(Self {
            staking_rpc,
            contract,
            stake,
        })
    }

    pub async fn update(&self) {}
}
