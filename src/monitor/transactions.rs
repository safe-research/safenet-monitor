use alloy::primitives::Address;
use prometheus::CounterVec;

/// Monitors Safenet consensus contract transactions and tracks attestation
/// outcomes as Prometheus metrics.
pub struct TransactionMonitor {
    consensus_rpc: String,
    contract: Address,
    transactions: CounterVec,
}

impl TransactionMonitor {
    pub fn new(consensus_rpc: String, contract: Address) -> Result<Self, prometheus::Error> {
        let transactions = prometheus::register_counter_vec!(
            "transactions",
            "Transactions observed on the Safenet consensus contract, \
             labelled by validity and attestation outcome.",
            &["status", "result"]
        )?;

        Ok(Self {
            consensus_rpc,
            contract,
            transactions,
        })
    }

    pub async fn update(&self) {}
}
