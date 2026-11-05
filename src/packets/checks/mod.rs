//! Basic static checks for the deterministic transaction security rules of the
//! Safenet Arbitration Charter (Article IV, Part A).
//!
//! These checks are intentionally not exhaustive: they only flag transactions
//! that are *obviously* insecure from their calldata alone, so that an oracle
//! approving them is a clear indication of misbehaving sentinels.

mod multi_send;

use alloy::{
    primitives::{Address, address},
    sol_types::SolCall as _,
};

use crate::bindings::IConsensus::SafeTransaction;
use multi_send::{SubTransaction, decode_multi_send, multi_send_contract};

mod bindings {
    alloy::sol! {
        function addOwnerWithThreshold(address owner, uint256 threshold);
        function removeOwner(address prevOwner, address owner, uint256 threshold);
        function swapOwner(address prevOwner, address oldOwner, address newOwner);
        function changeThreshold(uint256 threshold);
        function changeMasterCopy(address masterCopy);
        function setFallbackHandler(address handler);
        function setGuard(address guard);
        function enableModule(address module);
        function disableModule(address prevModule, address module);
        function setModuleGuard(address guard);
        function multiSend(bytes transactions);
    }
}

/// A deterministic charter rule that a transaction can violate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// R-4.1: the transaction changes a Safe setting outside of the allowed
    /// exceptions.
    SettingsChange,
    /// R-4.2: the transaction delegatecalls into an untrusted contract.
    Delegatecall,
}

impl Rule {
    pub const ALL: [Self; 2] = [Self::SettingsChange, Self::Delegatecall];

    /// The metric label for the rule.
    pub fn label(self) -> &'static str {
        match self {
            Self::SettingsChange => "settings_change",
            Self::Delegatecall => "delegatecall",
        }
    }

    /// The charter identifier for the rule.
    pub fn charter_id(self) -> &'static str {
        match self {
            Self::SettingsChange => "R-4.1",
            Self::Delegatecall => "R-4.2",
        }
    }
}

/// Safe functions that modify a Safe setting (§ 2.10) when called on the Safe
/// itself.
const SETTINGS_CHANGE_SELECTORS: &[[u8; 4]] = &[
    bindings::addOwnerWithThresholdCall::SELECTOR,
    bindings::removeOwnerCall::SELECTOR,
    bindings::swapOwnerCall::SELECTOR,
    bindings::changeThresholdCall::SELECTOR,
    bindings::changeMasterCopyCall::SELECTOR,
    bindings::setFallbackHandlerCall::SELECTOR,
    bindings::setGuardCall::SELECTOR,
    bindings::enableModuleCall::SELECTOR,
    bindings::disableModuleCall::SELECTOR,
    bindings::setModuleGuardCall::SELECTOR,
];

/// Contracts that can be delegatecalled without modifying Safe storage (other
/// than the `signedMessages` mapping, which is an allowed exception).
const TRUSTED_DELEGATECALL_TARGETS: &[Address] = &[
    // SignMessageLib
    address!("A65387F16B013cf2Af4605Ad8aA5ec25a2cbA3a2"), // 1.3.0 - canonical
    address!("98FFBBF51bb33A056B08ddf711f289936AafF717"), // 1.3.0 - eip155
    address!("d53cd0aB83D845Ac265BE939c57F53AD838012c9"), // 1.4.1
    address!("4FfeF8222648872B3dE295Ba1e49110E61f5b5aa"), // 1.5.0
    // CreateCall
    address!("7cbB62EaA69F79e6873cD1ecB2392971036cFAa4"), // 1.3.0 - canonical
    address!("B19D6FFc2182150F8Eb585b79D4ABcd7C5640A9d"), // 1.3.0 - eip155
    address!("9b35Af71d77eaf8d7e40252370304687390A1A52"), // 1.4.1
    address!("2Ef5ECfbea521449E4De05EDB1ce63B75eDA90B4"), // 1.5.0
];

/// Returns the charter rules that a proposed Safe transaction obviously
/// violates, or an empty list if none were detected.
pub fn check_transaction(tx: &SafeTransaction) -> Vec<Rule> {
    if is_allowed_settings_change(tx) {
        return Vec::new();
    }

    let mut violations = Vec::new();
    for call in transaction_calls(tx) {
        let rule = match call.operation {
            0 if call.to == tx.safe
                && call.data.get(..4).is_some_and(|selector| {
                    SETTINGS_CHANGE_SELECTORS.contains(&selector.try_into().unwrap())
                }) =>
            {
                Rule::SettingsChange
            }
            1 if !TRUSTED_DELEGATECALL_TARGETS.contains(&call.to) => Rule::Delegatecall,
            _ => continue,
        };
        if !violations.contains(&rule) {
            violations.push(rule);
        }
    }
    violations
}

/// Settings changes are only allowed as a non-batched, zero-value call to the
/// Safe itself, which either disables a module or removes the fallback handler.
fn is_allowed_settings_change(tx: &SafeTransaction) -> bool {
    if tx.to != tx.safe || tx.operation != 0 || !tx.value.is_zero() {
        return false;
    }

    // Require canonical ABI encoding by checking that decoding and re-encoding
    // the calldata round-trips.
    if let Ok(call) = bindings::disableModuleCall::abi_decode_validate(&tx.data) {
        return call.abi_encode() == tx.data[..];
    }
    if let Ok(call) = bindings::setFallbackHandlerCall::abi_decode_validate(&tx.data) {
        return call.handler.is_zero() && call.abi_encode() == tx.data[..];
    }
    false
}

/// Returns the calls the Safe makes when executing `tx`, with every
/// delegatecalled multi-send expanded into its sub-transactions, however
/// deeply nested.
///
/// Following Security Council precedent, a transaction is judged by what it
/// would do: a reverting call neither makes an insecure transaction secure nor
/// a secure one insecure. So calls are returned even if they, or other calls
/// in the transaction, would revert. Only calls that are never reached in any
/// state are left out (e.g. entries after a call-only multi-send's first
/// delegatecall, which it always reverts on).
///
/// Expansion uses an explicit stack instead of recursion, so deeply nested
/// multi-sends cannot exhaust the call stack.
fn transaction_calls(tx: &SafeTransaction) -> Vec<SubTransaction> {
    let mut calls = Vec::new();
    // Pending calls, next one on top. Sub-transactions are pushed in reverse
    // so they pop off in execution order.
    let mut pending = vec![SubTransaction {
        operation: tx.operation,
        to: tx.to,
        value: tx.value,
        data: tx.data.clone(),
    }];

    while let Some(call) = pending.pop() {
        // Only a delegatecall runs a multi-send in the Safe's context. A plain
        // call makes the sub-calls from the multi-send contract, so it stays a
        // single opaque call.
        let Some((allows_delegate_calls, version)) =
            multi_send_contract(call.to).filter(|_| call.operation == 1)
        else {
            calls.push(call);
            continue;
        };

        // Invalid `multiSend` call data makes the multi-send contract revert
        // without making any calls.
        let Ok(call) = bindings::multiSendCall::abi_decode(&call.data) else {
            continue;
        };
        let mut sub_txs = decode_multi_send(tx.safe, &call.transactions, version);
        // Call-only multi-sends revert at the first delegatecall, so it and
        // the entries after it are never reached.
        if !allows_delegate_calls
            && let Some(index) = sub_txs.iter().position(|sub_tx| sub_tx.operation == 1)
        {
            sub_txs.truncate(index);
        }
        pending.extend(sub_txs.into_iter().rev());
    }

    calls
}
