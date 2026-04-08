pub mod checks;

use alloy::{
    primitives::{Address, B256},
    sol,
    sol_types::{Eip712Domain, SolStruct as _, eip712_domain},
};

sol! {
    struct TransactionProposal {
        uint64 epoch;
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
    pub fn transaction_proposal(&self, epoch: u64, safe_tx_hash: B256) -> B256 {
        TransactionProposal {
            epoch,
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
        let domain =
            ConsensusDomain::new(100, address!("223624cBF099e5a8f8cD5aF22aFa424a1d1acEE9"));
        let message = domain.transaction_proposal(
            31643,
            b256!("03c23f7abc44935b2f11d79f5095813afb0bb8bb53d8f03b5ee0458ca9968dc7"),
        );
        assert_eq!(
            message,
            b256!("663d11b48bea51e182af1d9293cd00f3cd618ab4bedf62a78f2978f991670d0a"),
        );
    }
}
