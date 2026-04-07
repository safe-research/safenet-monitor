use prometheus::GaugeVec;

/// Monitors validator native token balances on the consensus chain and tracks
/// them as Prometheus metrics.
pub struct ValidatorBalances {
    consensus_rpc: String,
    balances: GaugeVec,
}

impl ValidatorBalances {
    pub fn new(consensus_rpc: String) -> Result<Self, prometheus::Error> {
        let balances = prometheus::register_gauge_vec!(
            "validator_balance",
            "Current native token balance per validator on the consensus chain.",
            &["validator"]
        )?;

        Ok(Self {
            consensus_rpc,
            balances,
        })
    }

    pub async fn update(&self) {}
}
