use std::collections::HashMap;

use alloy::{
    eips::BlockId,
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
    packets::{
        ConsensusDomain,
        checks::{self, Rule},
    },
};

struct Proposal {
    safe_tx_hash: B256,
    epoch: u64,
    oracle: Address,
    outcome: Option<Outcome>,
    violations: Vec<Rule>,
    oracle_deadline: u64,
    deadline: u64,
}

/// Monitors Safenet consensus contract transactions and tracks attestation
/// outcomes as Prometheus metrics.
pub struct TransactionAttestations {
    provider: Provider,
    contract: Address,
    oracles: Vec<Address>,
    domain: ConsensusDomain,
    oracle_timeout: u64,
    attestation_duration: u64,
    last_processed_block: Option<u64>,
    proposals: HashMap<B256, Proposal>,
    transactions: CounterVec,
    disputes: CounterVec,
    violations: CounterVec,
    blocks: IntGauge,
}

impl TransactionAttestations {
    pub async fn new(
        provider: Provider,
        contract: Address,
        oracles: Vec<Address>,
        oracle_timeout: u64,
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
        let disputes = prometheus::register_counter_vec_with_registry!(
            "disputes",
            "Oracle request disputes observed on the configured oracles, \
             labelled by arbitration status.",
            &["status"],
            registry
        )?;
        let violations = prometheus::register_counter_vec_with_registry!(
            "oracle_rule_violations",
            "Oracle approvals of transaction proposals that obviously violate \
             a deterministic Safenet charter rule, labelled by rule.",
            &["rule"],
            registry
        )?;
        let blocks = prometheus::register_int_gauge_with_registry!(
            "transactions_last_processed_block",
            "Last consensus chain block processed by the transaction attestation monitor.",
            registry
        )?;

        // Make sure to intialize all possible labels for our metric, so that
        // we have 0-values instead of "No data" in our queries.
        for status in ["approved", "rejected", "timeout", "disputed", "unfinalized"] {
            for result in ["attested", "unattested"] {
                transactions.with_label_values(&[status, result]).reset();
            }
        }
        for status in DISPUTE_STATUSES {
            disputes.with_label_values(&[status]).reset();
        }
        for rule in Rule::ALL {
            violations.with_label_values(&[rule.label()]).reset();
        }

        let chain_id = provider.get_chain_id().await?;
        let domain = ConsensusDomain::new(chain_id, contract);

        Ok(Self {
            provider,
            contract,
            oracles,
            domain,
            oracle_timeout,
            attestation_duration,
            last_processed_block: None,
            proposals: HashMap::new(),
            transactions,
            disputes,
            violations,
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

        // Query new transaction proposals and oracle outcomes. Logs are
        // returned in chain order, so a proposal is always registered before
        // its corresponding oracle outcome.
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
                    ISentinelOracle::RequestTimedOut::SIGNATURE_HASH,
                    ISentinelOracle::DisputeTriggered::SIGNATURE_HASH,
                    ISentinelOracle::DisputeResolved::SIGNATURE_HASH,
                    ISentinelOracle::DisputeOutOfScope::SIGNATURE_HASH,
                    ISentinelOracle::ArbitrationTimedOut::SIGNATURE_HASH,
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
                    Some(&ISentinelOracle::RequestTimedOut::SIGNATURE_HASH)
                        if self.oracles.contains(&log.address()) =>
                    {
                        self.on_request_timed_out(log)
                    }
                    Some(&ISentinelOracle::DisputeTriggered::SIGNATURE_HASH)
                        if self.oracles.contains(&log.address()) =>
                    {
                        self.on_dispute_triggered(log)
                    }
                    Some(&ISentinelOracle::DisputeResolved::SIGNATURE_HASH)
                        if self.oracles.contains(&log.address()) =>
                    {
                        self.on_dispute_resolved(log)
                    }
                    Some(&ISentinelOracle::DisputeOutOfScope::SIGNATURE_HASH)
                        if self.oracles.contains(&log.address()) =>
                    {
                        self.on_dispute_out_of_scope(log)
                    }
                    Some(&ISentinelOracle::ArbitrationTimedOut::SIGNATURE_HASH)
                        if self.oracles.contains(&log.address()) =>
                    {
                        self.on_arbitration_timed_out(log)
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
                .block(BlockId::latest())
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

                let status = status_label(proposal.outcome);
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

                match (proposal.outcome, attested) {
                    (Some(Outcome::Approved), false) => tracing::warn!(
                        safe_tx_hash =% proposal.safe_tx_hash,
                        epoch = proposal.epoch,
                        oracle =% proposal.oracle,
                        "an oracle approved transaction was not attested"
                    ),
                    (Some(Outcome::Approved), true) | (_, false) => {}
                    (_, true) => tracing::error!(
                        safe_tx_hash =% proposal.safe_tx_hash,
                        epoch = proposal.epoch,
                        oracle =% proposal.oracle,
                        status,
                        "a transaction not approved by the oracle was attested"
                    ),
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
        // Validators start waiting for the oracle in the proposal block (the
        // signing request is made in the same transaction), so measure the
        // deadlines from there.
        let block = log.block_number.unwrap_or(safe_block);
        let violations = checks::check_transaction(&event.transaction);
        if !violations.is_empty() {
            tracing::debug!(
                safe_tx_hash =% event.safeTxHash,
                epoch = event.epoch,
                oracle =% event.oracle,
                rules =? violations.iter().map(|rule| rule.label()).collect::<Vec<_>>(),
                "transaction proposal violates charter rules",
            );
        }
        let proposal = Proposal {
            safe_tx_hash: event.safeTxHash,
            epoch: event.epoch,
            oracle: event.oracle,
            outcome: None,
            violations,
            oracle_deadline: block + self.oracle_timeout,
            deadline: block + self.attestation_duration,
        };

        tracing::debug!(
            safe_tx_hash =% proposal.safe_tx_hash,
            epoch = proposal.epoch,
            oracle =% proposal.oracle,
            oracle_deadline = proposal.oracle_deadline,
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

        let outcome = if event.approved {
            Outcome::Approved
        } else {
            Outcome::Rejected
        };
        self.record_outcome(event.requestId, log, outcome);
    }

    fn on_request_timed_out(&mut self, log: &Log) {
        let event = match ISentinelOracle::RequestTimedOut::decode_log(&log.inner) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    transaction =? log.transaction_hash,
                    error =% err,
                    "failed to decode RequestTimedOut log",
                );
                return;
            }
        };

        self.record_outcome(event.requestId, log, Outcome::Timeout);
    }

    fn on_dispute_triggered(&mut self, log: &Log) {
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

        self.disputes.with_label_values(&["triggered"]).inc();
        self.record_outcome(event.requestId, log, Outcome::Disputed);
    }

    fn on_dispute_resolved(&mut self, log: &Log) {
        let event = match ISentinelOracle::DisputeResolved::decode_log(&log.inner) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    transaction =? log.transaction_hash,
                    error =% err,
                    "failed to decode DisputeResolved log",
                );
                return;
            }
        };

        // A dispute resolved in favour of approval was a false positive from
        // the denying sentinels, while one resolved in favour of denial
        // protected the user from a malicious transaction.
        let status = match event.outcome {
            SENTINEL_ORACLE_STATE_RESOLVED_APPROVED => "approved",
            SENTINEL_ORACLE_STATE_RESOLVED_DENIED => "denied",
            outcome => {
                tracing::warn!(
                    request_id =% event.requestId,
                    oracle =% log.address(),
                    outcome,
                    "unexpected dispute resolution outcome",
                );
                return;
            }
        };
        tracing::info!(
            request_id =% event.requestId,
            oracle =% log.address(),
            status,
            slashed =% event.slashed,
            context =% event.context,
            "dispute resolved by arbitration",
        );
        self.disputes.with_label_values(&[status]).inc();
    }

    fn on_dispute_out_of_scope(&mut self, log: &Log) {
        let event = match ISentinelOracle::DisputeOutOfScope::decode_log(&log.inner) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    transaction =? log.transaction_hash,
                    error =% err,
                    "failed to decode DisputeOutOfScope log",
                );
                return;
            }
        };

        tracing::info!(
            request_id =% event.requestId,
            oracle =% log.address(),
            context =% event.context,
            "dispute declined by arbitrator as out of scope",
        );
        self.disputes.with_label_values(&["out_of_scope"]).inc();
    }

    fn on_arbitration_timed_out(&mut self, log: &Log) {
        let event = match ISentinelOracle::ArbitrationTimedOut::decode_log(&log.inner) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    transaction =? log.transaction_hash,
                    error =% err,
                    "failed to decode ArbitrationTimedOut log",
                );
                return;
            }
        };

        tracing::warn!(
            request_id =% event.requestId,
            oracle =% log.address(),
            "dispute arbitration timed out",
        );
        self.disputes.with_label_values(&["timeout"]).inc();
    }

    fn record_outcome(&mut self, request_id: B256, log: &Log, outcome: Outcome) {
        // The oracle request ID is the transaction proposal message. Only
        // accept outcomes from the oracle that the proposal was made for.
        let oracle = log.address();
        let Some(proposal) = self
            .proposals
            .get_mut(&request_id)
            .filter(|proposal| proposal.oracle == oracle)
        else {
            tracing::debug!(
                request_id =% request_id,
                oracle =% oracle,
                outcome = outcome.label(),
                "ignoring oracle outcome for unknown transaction proposal",
            );
            return;
        };

        // An oracle approving a transaction that obviously violates the
        // charter is a strong indication of misbehaving sentinels. Record it
        // regardless of the oracle timeout, since the result is still final.
        if matches!(outcome, Outcome::Approved) {
            for rule in &proposal.violations {
                tracing::error!(
                    safe_tx_hash =% proposal.safe_tx_hash,
                    epoch = proposal.epoch,
                    oracle =% proposal.oracle,
                    rule = rule.label(),
                    charter_rule = rule.charter_id(),
                    "oracle approved a transaction that violates a rule",
                );
                self.violations.with_label_values(&[rule.label()]).inc();
            }
        }

        // Validators stop waiting for the oracle at the start of the oracle
        // deadline block (block timeouts are processed before the block's
        // events), so from their perspective, outcomes from that block onwards
        // mean the oracle request was never finalized.
        let block = log.block_number.unwrap_or(u64::MAX);
        if block >= proposal.oracle_deadline {
            tracing::debug!(
                safe_tx_hash =% proposal.safe_tx_hash,
                epoch = proposal.epoch,
                oracle =% proposal.oracle,
                outcome = outcome.label(),
                block,
                oracle_deadline = proposal.oracle_deadline,
                "ignoring oracle outcome after the oracle timeout",
            );
            return;
        }

        tracing::debug!(
            safe_tx_hash =% proposal.safe_tx_hash,
            epoch = proposal.epoch,
            oracle =% proposal.oracle,
            outcome = outcome.label(),
            "registering oracle outcome",
        );
        proposal.outcome = Some(outcome);
    }
}

/// `SentinelOracleRequest.State` values for resolved requests.
const SENTINEL_ORACLE_STATE_RESOLVED_APPROVED: u8 = 3;
const SENTINEL_ORACLE_STATE_RESOLVED_DENIED: u8 = 4;

/// Possible `status` labels for the disputes metric.
const DISPUTE_STATUSES: [&str; 5] = ["triggered", "approved", "denied", "out_of_scope", "timeout"];

/// The final state of an oracle request for a transaction proposal.
#[derive(Clone, Copy)]
enum Outcome {
    /// The oracle approved the transaction.
    Approved,
    /// The oracle rejected the transaction.
    Rejected,
    /// The oracle request timed out without a result.
    Timeout,
    /// The oracle request was disputed and requires arbitration.
    Disputed,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Timeout => "timeout",
            Self::Disputed => "disputed",
        }
    }
}

fn status_label(outcome: Option<Outcome>) -> &'static str {
    outcome.map_or("unfinalized", Outcome::label)
}

fn result_label(attested: bool) -> &'static str {
    if attested { "attested" } else { "unattested" }
}
