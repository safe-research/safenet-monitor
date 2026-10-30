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
    #[allow(dead_code, reason = "not needed by the checks, kept for completeness")]
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
///
/// This mirrors how the on-chain contracts execute a batch, so that the
/// returned sub-transactions are exactly the ones that may run:
///
/// - Entries starting in the last 32 bytes are never executed, as the
///   contracts loop with `lt(i, length)` where `i` starts at `0x20`.
/// - The contracts do not bounds check entries, and the memory following the
///   ABI decoded `transactions` is zeroed, so an entry that runs past the end
///   of the data is executed with its missing bytes read as zeros. It is
///   always the last executed entry, as the loop moves past the end after it.
/// - An invalid operation, or a data length so large that the call's memory
///   expansion exceeds any block gas limit, reverts the whole batch, so it is
///   an error.
pub fn decode_multi_send(
    safe: Address,
    data: &[u8],
    version: MultiSendVersion,
) -> Result<Vec<SubTransaction>> {
    let mut result = Vec::new();
    let mut cursor = Cursor(data);
    // The contracts loop while `lt(i, length)`, where `i` is offset by the
    // 32-byte length prefix but `length` is not, so an entry is only executed
    // if more than 32 bytes remain.
    while cursor.0.len() > 32 {
        let operation = cursor.read(1)[0];
        ensure!(operation <= 1, "invalid multi-send operation: {operation}");

        let to = Address::from_slice(&cursor.read(20));
        let value = U256::from_be_slice(&cursor.read(32));
        let data_len = U256::from_be_slice(&cursor.read(32))
            .try_into()
            .ok()
            .filter(|&len| len <= MAX_DATA_LENGTH)
            .context("multi-send data length too large")?;
        let data = Bytes::from(cursor.read(data_len));

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

/// Upper bound for a multi-send entry's data length. Calls with larger data
/// would expand memory far beyond what any block gas limit allows, so they
/// always revert. This also bounds the zero padding allocated for truncated
/// entries.
const MAX_DATA_LENGTH: usize = 16 << 20;

struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    /// Reads `len` bytes, padding with zeros past the end of the data like the
    /// on-chain contracts do.
    fn read(&mut self, len: usize) -> Vec<u8> {
        let (available, rest) = self.0.split_at(len.min(self.0.len()));
        self.0 = rest;
        let mut result = available.to_vec();
        result.resize(len, 0);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAFE: Address = address!("3850cd76006dc6CaCBCBB514995C47Ca8Ad0bb96");
    const TARGET: Address = address!("C92E8bdf79f0507f65a392b0ab4667716BFE0110");

    fn pack(operation: u8, to: Address, data_len: usize, data: &[u8]) -> Vec<u8> {
        let mut out = vec![operation];
        out.extend_from_slice(to.as_slice());
        out.extend_from_slice(&U256::from(1).to_be_bytes::<32>());
        out.extend_from_slice(&U256::from(data_len).to_be_bytes::<32>());
        out.extend_from_slice(data);
        out
    }

    fn decode(data: &[u8]) -> Vec<(u8, Address, Vec<u8>)> {
        decode_multi_send(SAFE, data, MultiSendVersion::V150Plus)
            .unwrap()
            .into_iter()
            .map(|tx| (tx.operation, tx.to, tx.data.to_vec()))
            .collect()
    }

    #[test]
    fn decodes_entries() {
        let data = [pack(0, TARGET, 2, &[1, 2]), pack(1, Address::ZERO, 0, &[])].concat();
        assert_eq!(decode(&data), [(0, TARGET, vec![1, 2]), (1, SAFE, vec![])]);
        assert_eq!(decode(&[]), []);
    }

    #[test]
    fn skips_entries_in_last_32_bytes() {
        let entry = pack(0, TARGET, 0, &[]);
        for len in [1, 32] {
            let data = [entry.clone(), vec![0xff; len]].concat();
            assert_eq!(decode(&data), [(0, TARGET, vec![])], "{len} trailing bytes");
        }
    }

    #[test]
    fn pads_truncated_entries_with_zeros() {
        let entry = pack(0, TARGET, 0, &[]);

        // 33 zero bytes are a call to `address(0)` (the Safe on v1.5.0+) with
        // zero value and empty data.
        let truncated = [entry.clone(), vec![0; 33]].concat();
        let txs = decode_multi_send(SAFE, &truncated, MultiSendVersion::V150Plus).unwrap();
        assert_eq!(txs.len(), 2);
        assert_eq!(
            (txs[1].to, txs[1].value, txs[1].data.len()),
            (SAFE, U256::ZERO, 0)
        );

        // An executed entry always has more than 32 bytes, so its operation
        // and address are complete, but its value can be truncated. Missing
        // bytes are zeros on the right.
        let truncated_value = [entry.clone(), vec![0], TARGET.to_vec(), vec![0xaa; 12]].concat();
        let txs = decode_multi_send(SAFE, &truncated_value, MultiSendVersion::V150Plus).unwrap();
        let mut value = [0; 32];
        value[..12].fill(0xaa);
        assert_eq!(
            (txs[1].to, txs[1].value),
            (TARGET, U256::from_be_bytes(value))
        );

        let short_data = [entry, pack(0, TARGET, 4, &[0xaa; 2])].concat();
        assert_eq!(
            decode(&short_data),
            [(0, TARGET, vec![]), (0, TARGET, vec![0xaa, 0xaa, 0, 0])],
        );
    }

    #[test]
    fn rejects_huge_data_lengths() {
        let data = pack(0, TARGET, MAX_DATA_LENGTH + 1, &[]);
        assert!(decode_multi_send(SAFE, &data, MultiSendVersion::V150Plus).is_err());
    }

    #[test]
    fn rejects_invalid_operations() {
        let data = [pack(0, TARGET, 0, &[]), pack(2, TARGET, 0, &[])].concat();
        assert!(decode_multi_send(SAFE, &data, MultiSendVersion::V150Plus).is_err());
    }
}
