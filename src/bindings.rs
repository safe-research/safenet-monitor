use alloy::sol;

sol! {
    interface IConsensus {
        struct SafeTransaction {
            uint256 chainId;
            address safe;
            address to;
            uint256 value;
            bytes data;
            uint8 operation;
            uint256 safeTxGas;
            uint256 baseGas;
            uint256 gasPrice;
            address gasToken;
            address refundReceiver;
            uint256 nonce;
        }

        event TransactionProposed(
            bytes32 indexed safeTxHash,
            uint256 indexed chainId,
            address indexed safe,
            uint64 epoch,
            SafeTransaction transaction
        );

        function getAttestationSignatureId(bytes32 message) external view returns (bytes32 signatureId);
        function getValidatorStaker(address validator) external view returns (address staker);
    }

    interface IStaking {
        function stakes(address staker, address validator) external view returns (uint256 amount);
        function totalStakedAmount() external view returns (uint256 amount);
    }
}
