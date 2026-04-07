use alloy::{
    primitives::Address, providers::RootProvider, rpc::client::ClientBuilder,
    transports::layers::RetryBackoffLayer,
};
use anyhow::Context as _;
use prometheus::{Encoder, TextEncoder};

pub mod balances;
pub mod stake;
pub mod transactions;

use balances::ValidatorBalances;
use stake::ValidatorStake;
use transactions::TransactionMonitor;

pub type Provider = RootProvider;

/// A named validator and its on-chain address.
#[derive(Clone)]
pub struct Validator {
    pub name: String,
    pub address: Address,
}

impl std::str::FromStr for Validator {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, address) = s
            .split_once(':')
            .with_context(|| format!("expected NAME:ADDRESS, got {s:?}"))?;
        let address = address
            .parse()
            .with_context(|| format!("invalid address {address:?}"))?;
        Ok(Self {
            name: name.to_owned(),
            address,
        })
    }
}

fn create_provider(url: String) -> Provider {
    let url = url.parse().expect("invalid RPC URL");
    let client = ClientBuilder::default()
        .layer(RetryBackoffLayer::new(10, 500, 20))
        .http(url);
    RootProvider::new(client)
}

struct Inner {
    transactions: TransactionMonitor,
    stake: ValidatorStake,
    balances: ValidatorBalances,
}

pub struct Monitor {
    inner: tokio::sync::Mutex<Inner>,
    registry: prometheus::Registry,
}

impl Monitor {
    pub fn new(
        consensus_rpc: String,
        staking_rpc: String,
        consensus_contract: Address,
        staking_contract: Address,
        validators: Vec<Validator>,
    ) -> Result<Self, prometheus::Error> {
        let registry = prometheus::Registry::new();

        let consensus = create_provider(consensus_rpc);
        let staking = create_provider(staking_rpc);

        let transactions =
            TransactionMonitor::new(consensus.clone(), consensus_contract, &registry)?;
        let stake = ValidatorStake::new(staking, staking_contract, validators.clone(), &registry)?;
        let balances = ValidatorBalances::new(consensus, validators, &registry)?;

        Ok(Self {
            inner: tokio::sync::Mutex::new(Inner {
                transactions,
                stake,
                balances,
            }),
            registry,
        })
    }

    pub async fn update(&self) {
        let Ok(mut inner) = self.inner.try_lock() else {
            // An update is already in progress; wait for it to finish so
            // callers get fresh metrics, but don't trigger another update.
            let _inner = self.inner.lock().await;
            return;
        };

        let Inner {
            transactions,
            stake,
            balances,
        } = &mut *inner;

        let (transactions, stake, balances) =
            tokio::join!(transactions.update(), stake.update(), balances.update(),);

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
