use prometheus::GaugeVec;

use super::Provider;

/// Monitors validator native token balances on the consensus chain and tracks
/// them as Prometheus metrics.
pub struct ValidatorBalances {
    provider: Provider,
    balances: GaugeVec,
}

impl ValidatorBalances {
    pub fn new(provider: Provider) -> Result<Self, prometheus::Error> {
        let balances = prometheus::register_gauge_vec!(
            "validator_balance",
            "Current native token balance per validator on the consensus chain.",
            &["validator"]
        )?;

        Ok(Self { provider, balances })
    }

    pub async fn update(&self) {}
}
