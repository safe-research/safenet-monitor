use alloy::{
    primitives::Address,
    providers::{CallItem, Provider as _},
    sol_types::SolCall as _,
};
use anyhow::{Context as _, Result};
use prometheus::GaugeVec;

use super::{Provider, Validator, utils};
use crate::bindings::{IConsensus, IStaking};

/// Monitors validator stake on the staking contract and tracks current stake
/// amounts as Prometheus metrics.
pub struct ValidatorStake {
    consensus: Provider,
    consensus_contract: Address,
    provider: Provider,
    contract: Address,
    validators: Vec<Validator>,
    stake: GaugeVec,
}

impl ValidatorStake {
    pub fn new(
        consensus: Provider,
        consensus_contract: Address,
        provider: Provider,
        contract: Address,
        validators: Vec<Validator>,
        registry: &prometheus::Registry,
    ) -> Result<Self> {
        let stake = prometheus::register_gauge_vec_with_registry!(
            "validator_stake",
            "Current stake amount per validator on the Safenet staking contract.",
            &["validator"],
            registry
        )?;

        Ok(Self {
            consensus,
            consensus_contract,
            provider,
            contract,
            validators,
            stake,
        })
    }

    pub async fn update(&mut self) -> Result<()> {
        // Fetch the staker address for each validator from the consensus contract.
        let staker_results = self
            .validators
            .iter()
            .fold(
                self.consensus.multicall().dynamic(),
                |multicall, validator| {
                    multicall.add_call_dynamic(
                        CallItem::<IConsensus::getValidatorStakerCall>::new(
                            self.consensus_contract,
                            IConsensus::getValidatorStakerCall {
                                validator: validator.address,
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
            .context("failed to fetch validator stakers")?;

        let validator_stakers: Vec<(&Validator, Address)> = self
            .validators
            .iter()
            .zip(staker_results)
            .filter_map(|(validator, result)| match result {
                Ok(staker) => Some((validator, staker)),
                Err(err) => {
                    tracing::warn!(
                        validator =% validator.name,
                        error =% err,
                        "failed to fetch validator staker",
                    );
                    None
                }
            })
            .collect();

        if validator_stakers.is_empty() {
            return Ok(());
        }

        // Fetch the stake amount for each (staker, validator) pair from the staking contract.
        let stake_results = validator_stakers
            .iter()
            .fold(
                self.provider.multicall().dynamic(),
                |multicall, (validator, staker)| {
                    multicall.add_call_dynamic(
                        CallItem::<IStaking::stakesCall>::new(
                            self.contract,
                            IStaking::stakesCall {
                                staker: *staker,
                                validator: validator.address,
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
            .context("failed to fetch stake amounts")?;

        for ((validator, _), result) in validator_stakers.iter().zip(stake_results) {
            let amount = match result {
                Ok(amount) => amount,
                Err(err) => {
                    tracing::warn!(
                        validator =% validator.name,
                        error =% err,
                        "failed to fetch validator stake",
                    );
                    continue;
                }
            };

            let approx_amount = utils::approx_units(amount);
            self.stake
                .with_label_values(&[&validator.name])
                .set(approx_amount);
            tracing::debug!(
                validator =% validator.name,
                amount =% approx_amount,
                "updated validator stake amount"
            )
        }

        Ok(())
    }
}
