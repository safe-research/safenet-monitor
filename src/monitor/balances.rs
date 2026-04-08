use alloy::{
    providers::{CallItem, MULTICALL3_ADDRESS, Provider as _, bindings::IMulticall3},
    sol_types::SolCall as _,
};
use anyhow::{Context as _, Result};
use prometheus::GaugeVec;

use super::{Provider, Validator, utils};

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
    ) -> Result<Self> {
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

    pub async fn update(&mut self) -> Result<()> {
        let balance_results = self
            .validators
            .iter()
            .fold(
                self.provider.multicall().dynamic(),
                |multicall, validator| {
                    multicall.add_call_dynamic(
                        CallItem::<IMulticall3::getEthBalanceCall>::new(
                            MULTICALL3_ADDRESS,
                            IMulticall3::getEthBalanceCall {
                                addr: validator.address,
                            }
                            .abi_encode()
                            .into(),
                        )
                        .allow_failure(true),
                    )
                },
            )
            .aggregate3()
            .await
            .context("failed to fetch balances")?;

        for (validator, result) in self.validators.iter().zip(balance_results) {
            let balance = match result {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!(
                        validator =% validator.name,
                        error =% err,
                        "failed to update validator balance",
                    );
                    continue;
                }
            };

            let approx_balance = utils::approx_units(balance);
            self.balances
                .with_label_values(&[&validator.name])
                .set(approx_balance);
            tracing::debug!(
                validator =% validator.name,
                balance =% approx_balance,
                "updated validator account native token balance"
            )
        }

        Ok(())
    }
}
