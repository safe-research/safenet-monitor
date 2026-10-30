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
use multi_send::{decode_multi_send, multi_send_contract};

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

/// A rule that a transaction can violate: either a deterministic charter rule
/// or a monitor specific rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// R-4.1: the transaction changes a Safe setting outside of the allowed
    /// exceptions.
    SettingsChange,
    /// R-4.2: the transaction delegatecalls into an untrusted contract.
    Delegatecall,
    /// Not a charter rule: the transaction nests multi-sends deeper than the
    /// monitor inspects, so it may hide charter violations.
    MultiSendDepth,
}

impl Rule {
    pub const ALL: [Self; 3] = [
        Self::SettingsChange,
        Self::Delegatecall,
        Self::MultiSendDepth,
    ];

    /// The metric label for the rule.
    pub fn label(self) -> &'static str {
        match self {
            Self::SettingsChange => "settings_change",
            Self::Delegatecall => "delegatecall",
            Self::MultiSendDepth => "multi_send_depth",
        }
    }

    /// The charter identifier for the rule, if it is a charter rule.
    pub fn charter_id(self) -> Option<&'static str> {
        match self {
            Self::SettingsChange => Some("R-4.1"),
            Self::Delegatecall => Some("R-4.2"),
            Self::MultiSendDepth => None,
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

/// Maximum depth of nested multi-sends that are inspected. The calldata is
/// attacker controlled and every level copies the inner batch, so deeper
/// batches are flagged with [`Rule::MultiSendDepth`] instead of inspected.
const MAX_MULTI_SEND_DEPTH: usize = 4;

/// Returns the charter rules that a proposed Safe transaction obviously
/// violates, or an empty list if none were detected.
pub fn check_transaction(tx: &SafeTransaction) -> Vec<Rule> {
    let mut violations = Vec::new();
    if !is_allowed_settings_change(tx) {
        check_call(tx.safe, tx.operation, tx.to, &tx.data, 0, &mut violations);
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

fn check_call(
    safe: Address,
    operation: u8,
    to: Address,
    data: &[u8],
    depth: usize,
    violations: &mut Vec<Rule>,
) {
    let mut violation = |rule| {
        if !violations.contains(&rule) {
            violations.push(rule);
        }
    };

    match operation {
        0 => {
            if to == safe
                && data.get(..4).is_some_and(|selector| {
                    SETTINGS_CHANGE_SELECTORS.contains(&selector.try_into().unwrap())
                })
            {
                violation(Rule::SettingsChange);
            }
        }
        1 => {
            if TRUSTED_DELEGATECALL_TARGETS.contains(&to) {
                return;
            }
            let Some((allows_delegate_calls, version)) = multi_send_contract(to) else {
                violation(Rule::Delegatecall);
                return;
            };

            if depth >= MAX_MULTI_SEND_DEPTH {
                violation(Rule::MultiSendDepth);
                return;
            }

            // Calls with invalid data or operations revert, and are not a
            // violation.
            let Ok(call) = bindings::multiSendCall::abi_decode(data) else {
                return;
            };
            let Ok(sub_txs) = decode_multi_send(safe, &call.transactions, version) else {
                return;
            };
            for sub_tx in sub_txs {
                // Call-only multi-sends revert on delegatecalls.
                if sub_tx.operation == 1 && !allows_delegate_calls {
                    continue;
                }
                check_call(
                    safe,
                    sub_tx.operation,
                    sub_tx.to,
                    &sub_tx.data,
                    depth + 1,
                    violations,
                );
            }
        }
        // Invalid operations revert.
        _ => {}
    }
}
