use std::mem;

use alloy::primitives::{Address, Bytes, U256};
use anyhow::{Context as _, Result, ensure};

use crate::bindings::IConsensus::SafeTransaction;

#[derive(Clone, Copy)]
pub enum MultiSendVersion {
    /// Legacy multi-send.
    Legacy,
    /// Safe v1.5.0+ where `to == address(0)` means a self-call.
    V150Plus,
}

/// Decodes a packed multi-send `transactions` byte blob into individual
/// sub-transactions. Each entry is:
///
/// ```text
/// uint8   operation
/// address to
/// uint256 value
/// uint256 dataLength
/// bytes   data (dataLength bytes)
/// ```
pub fn decode_multi_send(
    safe: Address,
    data: &[u8],
    version: MultiSendVersion,
) -> Result<Vec<SafeTransaction>> {
    let mut result = Vec::new();
    let mut cursor = Cursor(data);
    while let Some(operation) = cursor.next() {
        ensure!(operation <= 1, "invalid multi-send operation: {operation}");

        let (to, value, data) = Some(())
            .and_then(|_| {
                let to = Address::from_slice(cursor.read(20)?);
                let value = U256::from_be_slice(cursor.read(32)?);
                let data_len = U256::from_be_slice(cursor.read(32)?).try_into().ok()?;
                let data = Bytes::copy_from_slice(cursor.read(data_len)?);
                Some((to, value, data))
            })
            .context("invalid multi-send encoding")?;

        let to = match version {
            MultiSendVersion::V150Plus if to.is_zero() => safe,
            _ => to,
        };

        result.push(SafeTransaction {
            chainId: U256::ZERO,
            safe,
            to,
            value,
            data,
            operation,
            safeTxGas: U256::ZERO,
            baseGas: U256::ZERO,
            gasPrice: U256::ZERO,
            gasToken: Address::ZERO,
            refundReceiver: Address::ZERO,
            nonce: U256::ZERO,
        });
    }

    if let Some(last) = result.last() {
        // There is a known issue with the `beta` version of the validators,
        // where multisends where the last transaction has no data is considered
        // invalid (despite being valid). Make sure to replicate that behaviour
        // for computing metrics
        ensure!(
            !last.data.is_empty(),
            "final multi-send transaction with no data considered invalid"
        );
    }

    Ok(result)
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn next(&mut self) -> Option<u8> {
        Some(self.read(1)?[0])
    }

    fn read(&mut self, len: usize) -> Option<&'a [u8]> {
        let (result, rest) = mem::take(&mut self.0).split_at_checked(len)?;
        self.0 = rest;
        Some(result)
    }
}
