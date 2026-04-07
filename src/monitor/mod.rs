use alloy::{
    primitives::Address,
    providers::RootProvider,
    rpc::client::ClientBuilder,
    transports::layers::RetryBackoffLayer,
};
use prometheus::{Encoder, TextEncoder};

pub mod balances;
pub mod stake;
pub mod transactions;

use balances::ValidatorBalances;
use stake::ValidatorStake;
use transactions::TransactionMonitor;

pub type Provider = RootProvider;

fn create_provider(url: String) -> Provider {
    let url = url.parse().expect("invalid RPC URL");
    let client = ClientBuilder::default()
        .layer(RetryBackoffLayer::new(10, 500, 20))
        .http(url);
    RootProvider::new(client)
}

pub struct Monitor {
    registry: prometheus::Registry,
    transactions: TransactionMonitor,
    stake: ValidatorStake,
    balances: ValidatorBalances,
}

impl Monitor {
    pub fn new(
        consensus_rpc: String,
        staking_rpc: String,
        consensus_contract: Address,
        staking_contract: Address,
    ) -> Result<Self, prometheus::Error> {
        let registry = prometheus::Registry::new();

        let consensus = create_provider(consensus_rpc);
        let staking = create_provider(staking_rpc);

        let transactions =
            TransactionMonitor::new(consensus.clone(), consensus_contract, &registry)?;
        let stake = ValidatorStake::new(staking, staking_contract, &registry)?;
        let balances = ValidatorBalances::new(consensus, &registry)?;

        Ok(Self {
            registry,
            transactions,
            stake,
            balances,
        })
    }

    pub async fn update(&self) {
        let (transactions, stake, balances) = tokio::join!(
            self.transactions.update(),
            self.stake.update(),
            self.balances.update(),
        );

        if let Err(err) = transactions {
            tracing::error!(error = %err, "transaction monitor update failed");
        }
        if let Err(err) = stake {
            tracing::error!(error = %err, "stake monitor update failed");
        }
        if let Err(err) = balances {
            tracing::error!(error = %err, "balances monitor update failed");
        }
    }

    pub fn encode(&self) -> (String, Vec<u8>) {
        let encoder = TextEncoder::new();
        let families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&families, &mut buffer).unwrap();
        (encoder.format_type().to_owned(), buffer)
    }
}
