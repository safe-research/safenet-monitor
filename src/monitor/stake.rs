use alloy::primitives::Address;
use prometheus::GaugeVec;

use super::Provider;

/// Monitors validator stake on the staking contract and tracks current stake
/// amounts as Prometheus metrics.
pub struct ValidatorStake {
    provider: Provider,
    contract: Address,
    stake: GaugeVec,
}

impl ValidatorStake {
    pub fn new(
        provider: Provider,
        contract: Address,
        registry: &prometheus::Registry,
    ) -> Result<Self, prometheus::Error> {
        let stake = prometheus::register_gauge_vec_with_registry!(
            "validator_stake",
            "Current stake amount per validator on the Safenet staking contract.",
            &["validator"],
            registry
        )?;

        Ok(Self {
            provider,
            contract,
            stake,
        })
    }

    pub async fn update(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
