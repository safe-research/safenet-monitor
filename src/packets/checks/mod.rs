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

            // Calls with invalid data revert, and are not a violation.
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

#[cfg(test)]
mod tests {
    use alloy::{
        primitives::{Address, Bytes, U256, address, hex},
        sol,
        sol_types::SolCall as _,
    };

    use crate::bindings::IConsensus::SafeTransaction;

    use super::{Rule, bindings, check_transaction};

    sol! {
        function approve(address spender, uint256 amount);
        function signMessage(bytes message);
        function performCreate(uint256 value, bytes deploymentData);
        function migrateSingleton();
    }

    const SAFE: Address = address!("3850cd76006dc6CaCBCBB514995C47Ca8Ad0bb96");
    const TOKEN: Address = address!("9C58BAcC331c9aa871AFD802DB6379a98e80CEdb");
    const MULTI_SEND: Address = address!("218543288004CD07832472D464648173c77D7eB7");
    const MULTI_SEND_CALL_ONLY: Address = address!("40A2aCCbd92BCA938b02010E17A5b8929b49130D");
    const SIGN_MESSAGE_LIB: Address = address!("4FfeF8222648872B3dE295Ba1e49110E61f5b5aa");

    fn tx(to: Address, value: u64, data: impl Into<Bytes>, operation: u8) -> SafeTransaction {
        SafeTransaction {
            chainId: U256::from(100),
            safe: SAFE,
            to,
            value: U256::from(value),
            data: data.into(),
            operation,
            safeTxGas: U256::ZERO,
            baseGas: U256::ZERO,
            gasPrice: U256::ZERO,
            gasToken: Address::ZERO,
            refundReceiver: Address::ZERO,
            nonce: U256::ZERO,
        }
    }

    /// Packs a sub-transaction into the multi-send wire format.
    fn pack(operation: u8, to: Address, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(operation);
        out.extend_from_slice(to.as_slice());
        out.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
        out.extend_from_slice(&U256::from(data.len()).to_be_bytes::<32>());
        out.extend_from_slice(data);
        out
    }

    /// Encodes a `multiSend(bytes)` call wrapping the given packed sub-txs.
    fn multi_send(sub_txs: &[Vec<u8>]) -> Bytes {
        bindings::multiSendCall {
            transactions: sub_txs.concat().into(),
        }
        .abi_encode()
        .into()
    }

    fn approve_data() -> Vec<u8> {
        approveCall {
            spender: address!("C92E8bdf79f0507f65a392b0ab4667716BFE0110"),
            amount: U256::from(1_000_000_000_000_000_000_000_u128),
        }
        .abi_encode()
    }

    fn sign_message_data() -> Vec<u8> {
        signMessageCall {
            message: Bytes::from("swap GNO for SAFE"),
        }
        .abi_encode()
    }

    fn swap_owner_data() -> Vec<u8> {
        bindings::swapOwnerCall {
            prevOwner: address!("2dc63c83040669f0adba5f832f713152ba862c97"),
            oldOwner: address!("e7f8c378df23ebb06d5fc5a33bd471ef510f8cc9"),
            newOwner: address!("baf055b4ae60b897649f654df8def87bb4f86299"),
        }
        .abi_encode()
    }

    fn disable_module_data() -> Vec<u8> {
        bindings::disableModuleCall {
            prevModule: address!("0000000000000000000000000000000000000001"),
            module: address!("691f59471Bfd2B7d639DCF74671a2d648ED1E331"),
        }
        .abi_encode()
    }

    #[test]
    fn allows_regular_calls() {
        assert_eq!(check_transaction(&tx(TOKEN, 0, approve_data(), 0)), []);
        assert_eq!(check_transaction(&tx(TOKEN, 1, Bytes::new(), 0)), []);
    }

    #[test]
    fn allows_cancellation_transaction() {
        assert_eq!(check_transaction(&tx(SAFE, 0, Bytes::new(), 0)), []);
    }

    #[test]
    fn denies_owner_change() {
        assert_eq!(
            check_transaction(&tx(SAFE, 0, swap_owner_data(), 0)),
            [Rule::SettingsChange],
        );
    }

    #[test]
    fn allows_disable_module() {
        assert_eq!(
            check_transaction(&tx(SAFE, 0, disable_module_data(), 0)),
            []
        );
    }

    #[test]
    fn denies_disable_module_with_value() {
        assert_eq!(
            check_transaction(&tx(SAFE, 1, disable_module_data(), 0)),
            [Rule::SettingsChange],
        );
    }

    #[test]
    fn denies_disable_module_with_non_canonical_encoding() {
        let mut data = disable_module_data();
        data.extend_from_slice(&[0; 32]);
        assert_eq!(
            check_transaction(&tx(SAFE, 0, data, 0)),
            [Rule::SettingsChange],
        );
    }

    #[test]
    fn allows_removing_fallback_handler() {
        let data = bindings::setFallbackHandlerCall {
            handler: Address::ZERO,
        }
        .abi_encode();
        assert_eq!(check_transaction(&tx(SAFE, 0, data, 0)), []);
    }

    #[test]
    fn denies_setting_fallback_handler() {
        let data = bindings::setFallbackHandlerCall {
            handler: address!("fd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99"),
        }
        .abi_encode();
        assert_eq!(
            check_transaction(&tx(SAFE, 0, data, 0)),
            [Rule::SettingsChange],
        );
    }

    #[test]
    fn denies_settings_change_in_multi_send() {
        // Even allowed settings changes are not allowed when batched.
        for data in [swap_owner_data(), disable_module_data()] {
            let data = multi_send(&[pack(0, TOKEN, &approve_data()), pack(0, SAFE, &data)]);
            assert_eq!(
                check_transaction(&tx(MULTI_SEND_CALL_ONLY, 0, data, 1)),
                [Rule::SettingsChange],
            );
        }
    }

    #[test]
    fn denies_self_call_via_zero_address_in_v150_multi_send() {
        let data = multi_send(&[pack(
            0,
            Address::ZERO,
            &bindings::enableModuleCall {
                module: address!("5afe8f36504462aa6a7467372f9a41665820a14f"),
            }
            .abi_encode(),
        )]);
        assert_eq!(
            check_transaction(&tx(
                address!("A83c336B20401Af773B6219BA5027174338D1836"),
                0,
                data,
                1,
            )),
            [Rule::SettingsChange],
        );
    }

    #[test]
    fn allows_multi_send_with_regular_calls() {
        let data = multi_send(&[
            pack(0, TOKEN, &approve_data()),
            pack(0, address!("C92E8bdf79f0507f65a392b0ab4667716BFE0110"), &[]),
        ]);
        assert_eq!(check_transaction(&tx(MULTI_SEND_CALL_ONLY, 0, data, 1)), []);
        assert_eq!(
            check_transaction(&tx(MULTI_SEND_CALL_ONLY, 0, multi_send(&[]), 1)),
            []
        );
    }

    #[test]
    fn denies_batched_owner_changes() {
        let data = hex!(
            "8d80ff0a0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000022b0081a45aa50195f0a752159d5198780cdfb8e1973200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000064e318b52b0000000000000000000000003242071b0b406b6661af2de1115cd46567ab091700000000000000000000000007e5069f8f8e6a80432b13f20e9d4906de097e1a000000000000000000000000d80e356e94fb3f8e85b39b0c730fb7152e8cbd800081a45aa50195f0a752159d5198780cdfb8e1973200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000064f8dc5dd9000000000000000000000000d80e356e94fb3f8e85b39b0c730fb7152e8cbd80000000000000000000000000070941a8e2d7289e9594798f65a9379f6828d5bb00000000000000000000000000000000000000000000000000000000000000020081a45aa50195f0a752159d5198780cdfb8e1973200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000064f8dc5dd90000000000000000000000005afe8f36504462aa6a7467372f9a41665820a14f000000000000000000000000c0ffeee8baafa7ba6a6af2329892b88796cf44cf0000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000"
        );
        // Batched owner changes (swap + removes) on the Safe `0x81a4…9732`.
        let mut tx = tx(MULTI_SEND_CALL_ONLY, 0, data, 1);
        tx.safe = address!("81a45AA50195f0A752159d5198780cDfb8e19732");
        assert_eq!(check_transaction(&tx), [Rule::SettingsChange]);
    }

    #[test]
    fn allows_trusted_delegatecalls() {
        assert_eq!(
            check_transaction(&tx(SIGN_MESSAGE_LIB, 0, sign_message_data(), 1)),
            []
        );

        let data = performCreateCall {
            value: U256::ZERO,
            deploymentData: Bytes::from_static(&[0x60, 0x00, 0x60, 0x00, 0xf3]),
        }
        .abi_encode();
        assert_eq!(
            check_transaction(&tx(
                address!("9b35Af71d77eaf8d7e40252370304687390A1A52"),
                0,
                data,
                1,
            )),
            [],
        );
    }

    #[test]
    fn denies_bybit_transaction() {
        let data = hex!(
            "a9059cbb000000000000000000000000bdd077f651ebe7f7b3ce16fe5f2b025be29695160000000000000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            check_transaction(&tx(
                address!("96221423681A6d52E184D440a8eFCEbB105C7242"),
                0,
                data,
                1,
            )),
            [Rule::Delegatecall],
        );
    }

    #[test]
    fn denies_self_delegatecall() {
        assert_eq!(
            check_transaction(&tx(SAFE, 0, Bytes::new(), 1)),
            [Rule::Delegatecall],
        );
    }

    #[test]
    fn denies_singleton_migration() {
        assert_eq!(
            check_transaction(&tx(
                address!("526643F69b81B008F46d95CD5ced5eC0edFFDaC6"),
                0,
                migrateSingletonCall {}.abi_encode(),
                1,
            )),
            [Rule::Delegatecall],
        );
    }

    #[test]
    fn checks_delegatecalls_in_multi_send() {
        let trusted = multi_send(&[
            pack(1, SIGN_MESSAGE_LIB, &sign_message_data()),
            pack(0, TOKEN, &approve_data()),
        ]);
        assert_eq!(
            check_transaction(&tx(MULTI_SEND, 0, trusted.clone(), 1)),
            []
        );

        let untrusted = multi_send(&[
            pack(0, TOKEN, &approve_data()),
            pack(1, TOKEN, &approve_data()),
        ]);
        assert_eq!(
            check_transaction(&tx(MULTI_SEND, 0, untrusted.clone(), 1)),
            [Rule::Delegatecall],
        );

        // Call-only multi-sends revert on delegatecalls, so they can never
        // modify the Safe's storage.
        assert_eq!(
            check_transaction(&tx(MULTI_SEND_CALL_ONLY, 0, untrusted, 1)),
            []
        );
    }

    #[test]
    fn checks_nested_multi_sends() {
        let inner = multi_send(&[pack(1, TOKEN, &approve_data())]);
        let outer = multi_send(&[pack(1, MULTI_SEND, &inner)]);
        assert_eq!(
            check_transaction(&tx(MULTI_SEND, 0, outer, 1)),
            [Rule::Delegatecall],
        );
    }

    #[test]
    fn denies_too_deeply_nested_multi_sends() {
        let nested = |depth| {
            (0..depth).fold(
                multi_send(&[pack(0, TOKEN, &approve_data())]),
                |inner, _| multi_send(&[pack(1, MULTI_SEND, &inner)]),
            )
        };
        assert_eq!(check_transaction(&tx(MULTI_SEND, 0, nested(3), 1)), []);
        assert_eq!(
            check_transaction(&tx(MULTI_SEND, 0, nested(4), 1)),
            [Rule::MultiSendDepth],
        );
    }

    #[test]
    fn reports_multiple_violations() {
        let data = multi_send(&[
            pack(0, SAFE, &swap_owner_data()),
            pack(1, TOKEN, &approve_data()),
            pack(1, TOKEN, &approve_data()),
        ]);
        assert_eq!(
            check_transaction(&tx(MULTI_SEND, 0, data, 1)),
            [Rule::SettingsChange, Rule::Delegatecall],
        );
    }

    #[test]
    fn ignores_calls_to_multi_send() {
        // Calling (instead of delegatecalling) a multi-send executes the batch
        // in the context of the multi-send contract, not the Safe.
        let data = multi_send(&[pack(0, SAFE, &swap_owner_data())]);
        assert_eq!(check_transaction(&tx(MULTI_SEND, 0, data, 0)), []);
    }
}
