use std::collections::HashMap;

use alloy::{
    primitives::{Address, B256},
    providers::{CallItem, Provider as _},
    rpc::types::{BlockNumberOrTag, Filter, Log},
    sol_types::{SolCall as _, SolEvent as _},
};
use anyhow::{Context as _, Result};
use prometheus::{CounterVec, IntGauge};

use super::Provider;
use crate::{
    bindings::{IConsensus, IOracle, ISentinelOracle},
    packets::ConsensusDomain,
};

struct Proposal {
    safe_tx_hash: B256,
    epoch: u64,
    oracle: Address,
    approved: Option<bool>,
    deadline: u64,
}

/// Monitors Safenet consensus contract transactions and tracks attestation
/// outcomes as Prometheus metrics.
pub struct TransactionAttestations {
    provider: Provider,
    contract: Address,
    oracles: Vec<Address>,
    domain: ConsensusDomain,
    attestation_duration: u64,
    last_processed_block: Option<u64>,
    proposals: HashMap<B256, Proposal>,
    transactions: CounterVec,
    blocks: IntGauge,
}

impl TransactionAttestations {
    pub async fn new(
        provider: Provider,
        contract: Address,
        oracles: Vec<Address>,
        attestation_duration: u64,
        registry: &prometheus::Registry,
    ) -> Result<Self> {
        let transactions = prometheus::register_counter_vec_with_registry!(
            "transactions",
            "Transactions observed on the Safenet consensus contract, \
             labelled by oracle verdict and attestation outcome.",
            &["status", "result"],
            registry
        )?;
        let blocks = prometheus::register_int_gauge_with_registry!(
            "transactions_last_processed_block",
            "Last consensus chain block processed by the transaction attestation monitor.",
            registry
        )?;

        // Make sure to intialize all possible labels for our metric, so that
        // we have 0-values instead of "No data" in our queries.
        for status in ["approved", "rejected", "unresolved"] {
            for result in ["attested", "unattested"] {
                transactions.with_label_values(&[status, result]).reset();
            }
        }

        let chain_id = provider.get_chain_id().await?;
        let domain = ConsensusDomain::new(chain_id, contract);

        Ok(Self {
            provider,
            contract,
            oracles,
            domain,
            attestation_duration,
            last_processed_block: None,
            proposals: HashMap::new(),
            transactions,
            blocks,
        })
    }

    pub async fn update(&mut self) -> Result<()> {
        let safe_block = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Safe)
            .await
            .context("failed to get safe block")?
            .context("safe block not found")?
            .header
            .number;

        // Query new transaction proposals, oracle results and disputes. Logs are
        // returned in chain order, so a proposal is always registered before
        // its corresponding oracle result.
        let from_block = self
            .last_processed_block
            .unwrap_or(safe_block)
            .checked_add(1)
            .context("we have reached the heat death of the universe")?;
        if from_block <= safe_block {
            tracing::debug!(%from_block, to_block =% safe_block, "querying transaction events");
            let filter = Filter::new()
                .address(
                    std::iter::once(self.contract)
                        .chain(self.oracles.iter().copied())
                        .collect::<Vec<_>>(),
                )
                .event_signature(vec![
                    IConsensus::TransactionProposed::SIGNATURE_HASH,
                    IOracle::OracleResult::SIGNATURE_HASH,
                    ISentinelOracle::DisputeTriggered::SIGNATURE_HASH,
                ])
                .from_block(from_block)
                .to_block(safe_block);
            let logs = self
                .provider
                .get_logs(&filter)
                .await
                .context("failed to get transaction logs")?;

            for log in &logs {
                match log.topic0() {
                    Some(&IConsensus::TransactionProposed::SIGNATURE_HASH)
                        if log.address() == self.contract =>
                    {
                        self.on_transaction_proposed(log, safe_block)
                    }
                    Some(&IOracle::OracleResult::SIGNATURE_HASH)
                        if self.oracles.contains(&log.address()) =>
                    {
                        self.on_oracle_result(log)
                    }
                    Some(&ISentinelOracle::DisputeTriggered::SIGNATURE_HASH)
                        if self.oracles.contains(&log.address()) =>
                    {
                        self.on_dispute_triggered(log)
                    }
                    _ => {}
                }
            }
        }

        self.last_processed_block = Some(safe_block);
        self.blocks.set(safe_block as _);

        // Collect proposals past their attestation deadline.
        let due_messages: Vec<B256> = self
            .proposals
            .iter()
            .filter_map(|(msg, p)| (safe_block >= p.deadline).then_some(*msg))
            .collect();
        if !due_messages.is_empty() {
            let sig_results = due_messages
                .iter()
                .fold(self.provider.multicall().dynamic(), |multicall, message| {
                    multicall.add_call_dynamic(
                        CallItem::<IConsensus::getAttestationSignatureIdCall>::new(
                            self.contract,
                            IConsensus::getAttestationSignatureIdCall { message: *message }
                                .abi_encode()
                                .into(),
                        )
                        .allow_failure(true),
                    )
                })
                .aggregate3()
                .await
                .context("failed to fetch attestation signature IDs")?;

            for (message, result) in due_messages.iter().zip(sig_results) {
                let Some(proposal) = self.proposals.remove(message) else {
                    continue;
                };

                let attested = match result {
                    Ok(signature_id) => !signature_id.is_zero(),
                    Err(err) => {
                        tracing::warn!(
                            message =% message,
                            error =% err,
                            "failed to get attestation signature ID",
                        );
                        // Re-insert so we retry on the next update.
                        self.proposals.insert(*message, proposal);
                        continue;
                    }
                };

                let status = status_label(proposal.approved);
                let result = result_label(attested);
                tracing::debug!(
                    safe_tx_hash =% proposal.safe_tx_hash,
                    epoch = proposal.epoch,
                    oracle =% proposal.oracle,
                    status,
                    result,
                    "recording transaction attestation outcome",
                );
                self.transactions.with_label_values(&[status, result]).inc();

                match (proposal.approved, attested) {
                    (Some(true), false) => tracing::warn!(
                        safe_tx_hash =% proposal.safe_tx_hash,
                        epoch = proposal.epoch,
                        oracle =% proposal.oracle,
                        "an oracle approved transaction was not attested"
                    ),
                    (Some(false) | None, true) => tracing::error!(
                        safe_tx_hash =% proposal.safe_tx_hash,
                        epoch = proposal.epoch,
                        oracle =% proposal.oracle,
                        status,
                        "a transaction not approved by the oracle was attested"
                    ),
                    _ => {}
                }
            }
        }

        Ok(())
    }

    fn on_transaction_proposed(&mut self, log: &Log, safe_block: u64) {
        let event = match IConsensus::TransactionProposed::decode_log(&log.inner) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    transaction =? log.transaction_hash,
                    error =% err,
                    "failed to decode TransactionProposed log",
                );
                return;
            }
        };

        // Validators only attest to transactions evaluated by oracles that
        // they trust, so ignore proposals for any other oracle.
        if !self.oracles.contains(&event.oracle) {
            tracing::debug!(
                safe_tx_hash =% event.safeTxHash,
                oracle =% event.oracle,
                "ignoring transaction proposal for unknown oracle",
            );
            return;
        }

        let message = self.domain.transaction_proposal(
            event.epoch,
            event.oracle,
            event.oracleData.clone(),
            event.safeTxHash,
        );
        let proposal = Proposal {
            safe_tx_hash: event.safeTxHash,
            epoch: event.epoch,
            oracle: event.oracle,
            approved: None,
            deadline: safe_block + self.attestation_duration,
        };

        tracing::debug!(
            safe_tx_hash =% proposal.safe_tx_hash,
            epoch = proposal.epoch,
            oracle =% proposal.oracle,
            deadline = proposal.deadline,
            "registering transaction proposal",
        );
        self.proposals.entry(message).or_insert(proposal);
    }

    fn on_oracle_result(&mut self, log: &Log) {
        let event = match IOracle::OracleResult::decode_log(&log.inner) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    transaction =? log.transaction_hash,
                    error =% err,
                    "failed to decode OracleResult log",
                );
                return;
            }
        };

        // The oracle request ID is the transaction proposal message. Only
        // accept results from the oracle that the proposal was made for.
        let Some(proposal) = self
            .proposals
            .get_mut(&event.requestId)
            .filter(|proposal| proposal.oracle == log.address())
        else {
            tracing::debug!(
                request_id =% event.requestId,
                oracle =% log.address(),
                "ignoring oracle result for unknown transaction proposal",
            );
            return;
        };

        tracing::debug!(
            safe_tx_hash =% proposal.safe_tx_hash,
            epoch = proposal.epoch,
            oracle =% proposal.oracle,
            approved = event.approved,
            "registering oracle result",
        );
        proposal.approved = Some(event.approved);
    }

    fn on_dispute_triggered(&self, log: &Log) {
        let event = match ISentinelOracle::DisputeTriggered::decode_log(&log.inner) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    transaction =? log.transaction_hash,
                    error =% err,
                    "failed to decode DisputeTriggered log",
                );
                return;
            }
        };

        // Disputed requests do not produce an oracle result that validators
        // attest to.
        let proposal = self.proposals.get(&event.requestId);
        tracing::info!(
            request_id =% event.requestId,
            oracle =% log.address(),
            arbitration_deadline = event.deadline,
            safe_tx_hash =? proposal.map(|p| p.safe_tx_hash),
            epoch =? proposal.map(|p| p.epoch),
            "transaction proposal disputed and requires arbitration",
        );
    }
}

fn status_label(approved: Option<bool>) -> &'static str {
    match approved {
        Some(true) => "approved",
        Some(false) => "rejected",
        None => "unresolved",
    }
}

fn result_label(attested: bool) -> &'static str {
    if attested { "attested" } else { "unattested" }
}
