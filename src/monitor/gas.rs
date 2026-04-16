use alloy::{providers::Provider as _, rpc::types::BlockNumberOrTag};
use anyhow::{Context as _, Result};
use prometheus::{Gauge, GaugeVec};

use crate::monitor::utils;

use super::Provider;

/// Monitors gas fees on the consensus chain and tracks them as Prometheus metrics.
pub struct GasFees {
    provider: Provider,
    base_gas_fee: Gauge,
    priority_fees: GaugeVec,
}

static PRIORITY_FEE_PERCENTILES: &[f64] = &[25., 50., 75.];

impl GasFees {
    pub fn new(provider: Provider, registry: &prometheus::Registry) -> Result<Self> {
        let base_gas_fee = prometheus::register_gauge_with_registry!(
            "base_gas_fee",
            "Current base gas fee on the consensus chain, in Gwei.",
            registry
        )?;

        let priority_fees = prometheus::register_gauge_vec_with_registry!(
            "priority_fee",
            "Current priority fee on the consensus chain by percentile, in Gwei.",
            &["percentile"],
            registry
        )?;

        Ok(Self {
            provider,
            base_gas_fee,
            priority_fees,
        })
    }

    pub async fn update(&mut self) -> Result<()> {
        let fee_history = self
            .provider
            .get_fee_history(1, BlockNumberOrTag::Latest, PRIORITY_FEE_PERCENTILES)
            .await
            .context("failed to get fee history")?;

        let base_fee = fee_history
            .latest_block_base_fee()
            .context("fee history returned no base fees")?;

        self.base_gas_fee.set(utils::approx_gwei(base_fee));
        tracing::debug!(base_fee, "updated base gas fee");

        if let Some(rewards) = fee_history.reward.as_deref().and_then(<[_]>::first) {
            for (&percentile, &reward) in PRIORITY_FEE_PERCENTILES.iter().zip(rewards.iter()) {
                let label = format!("p{}", percentile as u64);
                self.priority_fees
                    .with_label_values(&[&label])
                    .set(utils::approx_gwei(reward));
            }
            tracing::debug!(?rewards, "updated priority fees");
        }

        Ok(())
    }
}
