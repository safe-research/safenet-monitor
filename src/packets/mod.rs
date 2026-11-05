// TODO: remove once the checks are used by the transaction monitor.
#[allow(dead_code)]
pub mod checks;

use alloy::{
    primitives::{Address, B256, Bytes},
    sol,
    sol_types::{Eip712Domain, SolStruct as _, eip712_domain},
};

sol! {
    struct TransactionProposal {
        uint64 epoch;
        address oracle;
        bytes oracleData;
        bytes32 safeTxHash;
    }
}

/// EIP-712 domain for a Safenet consensus contract instance.
pub struct ConsensusDomain(Eip712Domain);

impl ConsensusDomain {
    /// Creates a new consensus domain for the given chain ID and contract address.
    pub fn new(chain_id: u64, address: Address) -> Self {
        Self(eip712_domain! {
            chain_id: chain_id,
            verifying_contract: address,
        })
    }

    /// Computes the EIP-712 signing hash for a `TransactionProposal` message.
    ///
    /// This is the message that validators attest to, and is also used as the
    /// request ID for the oracle evaluating the transaction.
    pub fn transaction_proposal(
        &self,
        epoch: u64,
        oracle: Address,
        oracle_data: Bytes,
        safe_tx_hash: B256,
    ) -> B256 {
        TransactionProposal {
            epoch,
            oracle,
            oracleData: oracle_data,
            safeTxHash: safe_tx_hash,
        }
        .eip712_signing_hash(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256};

    #[test]
    fn transaction_proposal_message() {
        let domain = ConsensusDomain::new(1, address!("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045"));
        let message = domain.transaction_proposal(
            1,
            address!("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
            Bytes::new(),
            b256!("fe8b85e8d090b16fe8f142d3c9292dc1fc77daf9eb4af8f7cf4a7707d95f4028"),
        );
        assert_eq!(
            message,
            b256!("5ac4a916cb21b51c5b61c6d4479f7c7477856abecb96e439fb53d5462fb5b3ed"),
        );
    }
}
