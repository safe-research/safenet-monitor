use alloy::sol;

sol! {
    interface IConsensus {
        function getValidatorStaker(address validator) external view returns (address staker);
    }

    interface IStaking {
        function stakes(address staker, address validator) external view returns (uint256 amount);
    }
}
