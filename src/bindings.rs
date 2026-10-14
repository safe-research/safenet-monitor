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
            bytes32 indexed safeId,
            address indexed oracle,
            uint64 epoch,
            bytes oracleData,
            SafeTransaction transaction
        );

        function getAttestationSignatureId(bytes32 message) external view returns (bytes32 signatureId);
        function getValidatorStaker(address validator) external view returns (address staker);
    }

    interface IOracle {
        event OracleResult(bytes32 indexed requestId, address indexed sponsor, bytes result, bool approved);
    }

    interface ISentinelOracle {
        event DisputeTriggered(bytes32 indexed requestId, uint64 deadline);
    }

    interface IStaking {
        function stakes(address staker, address validator) external view returns (uint256 amount);
        function totalStakedAmount() external view returns (uint256 amount);
    }
}
