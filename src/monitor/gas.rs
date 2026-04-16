use alloy::{providers::Provider as _, rpc::types::BlockNumberOrTag};
use anyhow::{Context as _, Result};
use prometheus::Gauge;

use super::Provider;

/// Monitors the base gas fee on the consensus chain and tracks it as a
/// Prometheus metric.
pub struct BaseGasFee {
    provider: Provider,
    base_gas_fee: Gauge,
}

impl BaseGasFee {
    pub fn new(provider: Provider, registry: &prometheus::Registry) -> Result<Self> {
        let base_gas_fee = prometheus::register_gauge_with_registry!(
            "base_gas_fee",
            "Current base gas fee on the consensus chain, in Gwei.",
            registry
        )?;

        Ok(Self {
            provider,
            base_gas_fee,
        })
    }

    pub async fn update(&mut self) -> Result<()> {
        let fee_history = self
            .provider
            .get_fee_history(1, BlockNumberOrTag::Latest, &[])
            .await
            .context("failed to get fee history")?;

        let base_fee = fee_history
            .base_fee_per_gas
            .first()
            .context("fee history returned no base fees")?;

        let base_fee_gwei = *base_fee as f64 / 1e9;
        self.base_gas_fee.set(base_fee_gwei);
        tracing::debug!(base_fee_gwei, "updated base gas fee");

        Ok(())
    }
}
