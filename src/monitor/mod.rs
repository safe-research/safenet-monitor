use alloy::{
    primitives::Address, providers::RootProvider, rpc::client::ClientBuilder,
    transports::layers::RetryBackoffLayer,
};
use anyhow::{Context as _, Result};
use prometheus::{Encoder, TextEncoder};

mod utils;

pub mod balances;
pub mod stake;
pub mod transactions;

use balances::ValidatorBalances;
use stake::ValidatorStake;
use transactions::TransactionAttestations;

pub type Provider = RootProvider;

/// A named validator and its on-chain address.
#[derive(Clone)]
pub struct Validator {
    pub name: String,
    pub address: Address,
}

impl std::str::FromStr for Validator {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (name, address) = s
            .split_once('@')
            .with_context(|| format!("expected NAME@ADDRESS, got {s:?}"))?;
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
    transactions: TransactionAttestations,
    stake: ValidatorStake,
    balances: ValidatorBalances,
}

pub struct Monitor {
    inner: tokio::sync::Mutex<Inner>,
    registry: prometheus::Registry,
}

impl Monitor {
    pub async fn new(
        consensus_rpc: String,
        staking_rpc: String,
        consensus_contract: Address,
        staking_contract: Address,
        validators: Vec<Validator>,
        attestation_duration: u64,
    ) -> Result<Self> {
        let registry = prometheus::Registry::new_custom(
            Some("safenet_monitor".to_string()),
            Default::default(),
        )?;

        let consensus = create_provider(consensus_rpc);
        let staking = create_provider(staking_rpc);

        let transactions = TransactionAttestations::new(
            consensus.clone(),
            consensus_contract,
            attestation_duration,
            &registry,
        )
        .await?;
        let stake = ValidatorStake::new(
            consensus.clone(),
            consensus_contract,
            staking,
            staking_contract,
            validators.clone(),
            &registry,
        )?;
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
            tokio::join!(transactions.update(), stake.update(), balances.update());

        if let Err(err) = transactions {
            tracing::warn!(error = %err, "transactions update failed");
        }
        if let Err(err) = stake {
            tracing::warn!(error = %err, "validator stake update failed");
        }
        if let Err(err) = balances {
            tracing::warn!(error = %err, "validator balances update failed");
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
