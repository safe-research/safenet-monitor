use alloy::{
    network::TransactionBuilder as _, primitives::Address, providers::Provider as _,
    rpc::types::TransactionRequest, sol_types::SolCall as _,
};
use anyhow::{Context as _, Result};
use prometheus::Gauge;

use super::{Provider, utils};

mod bindings {
    alloy::sol! {
        interface ICumulativeMerkleDrop {
            function token() external view returns (address token);
        }

        interface IERC20 {
            function balanceOf(address account) external view returns (uint256 balance);
        }
    }
}

/// Monitors the rewards token balance held by the CumulativeMerkleDrop
/// contract.
pub struct CumulativeMerkleDropBalance {
    provider: Provider,
    contract: Address,
    token: Address,
    balance: Gauge,
}

impl CumulativeMerkleDropBalance {
    pub async fn new(
        provider: Provider,
        contract: Address,
        registry: &prometheus::Registry,
    ) -> Result<Self> {
        let token = Self::fetch_token(&provider, contract).await?;

        let balance = prometheus::register_gauge_with_registry!(
            "cumulative_merkle_drop_balance",
            "Current token balance held by the CumulativeMerkleDrop contract.",
            registry
        )?;

        Ok(Self {
            provider,
            contract,
            token,
            balance,
        })
    }

    pub async fn update(&mut self) -> Result<()> {
        let balance = self.fetch_balance().await?;

        let approx_balance = utils::approx_units(balance);
        self.balance.set(approx_balance);
        tracing::debug!(
            contract =% self.contract,
            token =% self.token,
            balance =% approx_balance,
            "updated cumulative merkle drop token balance",
        );

        Ok(())
    }

    async fn fetch_token(provider: &Provider, contract: Address) -> Result<Address> {
        let tx = TransactionRequest::default()
            .with_to(contract)
            .with_input(bindings::ICumulativeMerkleDrop::tokenCall {}.abi_encode());
        let raw = provider
            .call(tx)
            .await
            .context("failed to fetch CumulativeMerkleDrop token")?;

        bindings::ICumulativeMerkleDrop::tokenCall::abi_decode_returns(&raw)
            .context("failed to decode CumulativeMerkleDrop token")
    }

    async fn fetch_balance(&self) -> Result<alloy::primitives::U256> {
        let tx = TransactionRequest::default()
            .with_to(self.token)
            .with_input(
                bindings::IERC20::balanceOfCall {
                    account: self.contract,
                }
                .abi_encode(),
            );
        let raw = self
            .provider
            .call(tx)
            .await
            .context("failed to fetch CumulativeMerkleDrop token balance")?;

        bindings::IERC20::balanceOfCall::abi_decode_returns(&raw)
            .context("failed to decode CumulativeMerkleDrop token balance")
    }
}
