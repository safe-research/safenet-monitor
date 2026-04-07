use alloy::primitives::Address;
use prometheus::CounterVec;

use super::Provider;

/// Monitors Safenet consensus contract transactions and tracks attestation
/// outcomes as Prometheus metrics.
pub struct TransactionMonitor {
    provider: Provider,
    contract: Address,
    transactions: CounterVec,
}

impl TransactionMonitor {
    pub fn new(
        provider: Provider,
        contract: Address,
        registry: &prometheus::Registry,
    ) -> Result<Self, prometheus::Error> {
        let transactions = prometheus::register_counter_vec_with_registry!(
            "transactions",
            "Transactions observed on the Safenet consensus contract, \
             labelled by validity and attestation outcome.",
            &["status", "result"],
            registry
        )?;

        Ok(Self {
            provider,
            contract,
            transactions,
        })
    }

    pub async fn update(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}
