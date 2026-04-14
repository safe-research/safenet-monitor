use std::collections::HashMap;

use alloy::{
    primitives::{Address, B256},
    providers::{CallItem, Provider as _},
    rpc::types::{BlockNumberOrTag, Filter},
    sol_types::{SolCall as _, SolEvent as _},
};
use anyhow::{Context as _, Result};
use prometheus::{CounterVec, IntGauge};

use super::Provider;
use crate::{
    bindings::IConsensus,
    packets::{ConsensusDomain, checks},
};

struct Proposal {
    safe_tx_hash: B256,
    epoch: u64,
    valid: bool,
    deadline: u64,
}

/// Monitors Safenet consensus contract transactions and tracks attestation
/// outcomes as Prometheus metrics.
pub struct TransactionAttestations {
    provider: Provider,
    contract: Address,
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
        attestation_duration: u64,
        registry: &prometheus::Registry,
    ) -> Result<Self> {
        let transactions = prometheus::register_counter_vec_with_registry!(
            "transactions",
            "Transactions observed on the Safenet consensus contract, \
             labelled by validity and attestation outcome.",
            &["status", "result"],
            registry
        )?;
        let blocks = prometheus::register_int_gauge_with_registry!(
            "transactions_last_processed_block",
            "Last consensus chain block processed by the transaction attestation monitor.",
            registry
        )?;
        let chain_id = provider.get_chain_id().await?;
        let domain = ConsensusDomain::new(chain_id, contract);

        Ok(Self {
            provider,
            contract,
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

        // Query new transaction proposals.
        let from_block = self
            .last_processed_block
            .unwrap_or(safe_block)
            .checked_add(1)
            .context("we have reached the heat death of the universe")?;
        if from_block <= safe_block {
            tracing::debug!(%from_block, to_block =% safe_block, "querying transaction proposals");
            let filter = Filter::new()
                .address(self.contract)
                .event(IConsensus::TransactionProposed::SIGNATURE)
                .from_block(from_block)
                .to_block(safe_block);
            let logs = self
                .provider
                .get_logs(&filter)
                .await
                .context("failed to get TransactionProposed logs")?;

            for log in &logs {
                let event = match IConsensus::TransactionProposed::decode_log(&log.inner) {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::warn!(
                            transaction =? log.transaction_hash,
                            error =% err,
                            "failed to decode TransactionProposed log",
                        );
                        continue;
                    }
                };

                let message = self
                    .domain
                    .transaction_proposal(event.epoch, event.safeTxHash);
                let proposal = Proposal {
                    safe_tx_hash: event.safeTxHash,
                    epoch: event.epoch,
                    valid: checks::check_transaction(&event.transaction),
                    deadline: safe_block + self.attestation_duration,
                };

                tracing::debug!(
                    safe_tx_hash =% proposal.safe_tx_hash,
                    epoch = proposal.epoch,
                    valid = proposal.valid,
                    deadline = proposal.deadline,
                    "registering transaction proposal",
                );
                self.proposals.entry(message).or_insert(proposal);
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

                tracing::debug!(
                    safe_tx_hash =% proposal.safe_tx_hash,
                    epoch = proposal.epoch,
                    status = status_label(proposal.valid),
                    result = result_label(attested),
                    "recording transaction attestation outcome",
                );
                self.transactions
                    .with_label_values(&[status_label(proposal.valid), result_label(attested)])
                    .inc();

                match (proposal.valid, attested) {
                    (true, false) => tracing::warn!(
                        safe_tx_hash =% proposal.safe_tx_hash,
                        epoch = proposal.epoch,
                        "a valid transaction was not attested"
                    ),
                    (false, true) => tracing::error!(
                        safe_tx_hash =% proposal.safe_tx_hash,
                        epoch = proposal.epoch,
                        "an invalid transaction was attested"
                    ),
                    _ => {}
                }
            }
        }

        Ok(())
    }
}

fn status_label(valid: bool) -> &'static str {
    if valid { "valid" } else { "invalid" }
}

fn result_label(attested: bool) -> &'static str {
    if attested { "attested" } else { "unattested" }
}
