use prometheus::GaugeVec;

use super::{Provider, Validator};

/// Monitors validator native token balances on the consensus chain and tracks
/// them as Prometheus metrics.
pub struct ValidatorBalances {
    provider: Provider,
    validators: Vec<Validator>,
    balances: GaugeVec,
}

impl ValidatorBalances {
    pub fn new(
        provider: Provider,
        validators: Vec<Validator>,
        registry: &prometheus::Registry,
    ) -> Result<Self, prometheus::Error> {
        let balances = prometheus::register_gauge_vec_with_registry!(
            "validator_balance",
            "Current native token balance per validator on the consensus chain.",
            &["validator"],
            registry
        )?;

        Ok(Self {
            provider,
            validators,
            balances,
        })
    }

    pub async fn update(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
