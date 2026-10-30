use std::mem;

use alloy::primitives::{Address, Bytes, U256, address};
use anyhow::{Context as _, Result, ensure};

#[derive(Clone, Copy)]
pub enum MultiSendVersion {
    /// Legacy multi-send.
    Legacy,
    /// Safe v1.5.0+ where `to == address(0)` means a self-call.
    V150Plus,
}

/// A single sub-transaction of a multi-send batch.
pub struct SubTransaction {
    pub operation: u8,
    pub to: Address,
    #[expect(dead_code, reason = "not needed by the checks, kept for completeness")]
    pub value: U256,
    pub data: Bytes,
}

/// Returns whether the multi-send contract allows delegatecalls and its
/// encoding version, or `None` if `to` is not a known multi-send contract.
pub fn multi_send_contract(to: Address) -> Option<(bool, MultiSendVersion)> {
    let contract = match to {
        // 1.5.0
        a if a == address!("218543288004CD07832472D464648173c77D7eB7") => {
            (true, MultiSendVersion::V150Plus)
        }
        a if a == address!("A83c336B20401Af773B6219BA5027174338D1836") => {
            (false, MultiSendVersion::V150Plus)
        }
        // 1.4.1
        a if a == address!("38869bf66a61cF6bDB996A6aE40D5853Fd43B526") => {
            (true, MultiSendVersion::Legacy)
        }
        a if a == address!("9641d764fc13c8B624c04430C7356C1C7C8102e2") => {
            (false, MultiSendVersion::Legacy)
        }
        // 1.3.0 - canonical
        a if a == address!("A238CBeb142c10Ef7Ad8442C6D1f9E89e07e7761") => {
            (true, MultiSendVersion::Legacy)
        }
        a if a == address!("40A2aCCbd92BCA938b02010E17A5b8929b49130D") => {
            (false, MultiSendVersion::Legacy)
        }
        // 1.3.0 - eip155
        a if a == address!("998739BFdAAdde7C933B942a68053933098f9EDa") => {
            (true, MultiSendVersion::Legacy)
        }
        a if a == address!("A1dabEF33b3B82c7814B6D82A79e50F4AC44102B") => {
            (false, MultiSendVersion::Legacy)
        }
        _ => return None,
    };
    Some(contract)
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
) -> Result<Vec<SubTransaction>> {
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

        result.push(SubTransaction {
            operation,
            to,
            value,
            data,
        });
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
