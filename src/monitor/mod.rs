use alloy::{
    providers::RootProvider,
    rpc::client::ClientBuilder,
    transports::layers::RetryBackoffLayer,
};

pub mod balances;
pub mod stake;
pub mod transactions;

pub type Provider = RootProvider;

pub fn create_provider(url: String) -> Provider {
    let url = url.parse().expect("invalid RPC URL");
    let client = ClientBuilder::default()
        .layer(RetryBackoffLayer::new(10, 500, 20))
        .http(url);
    RootProvider::new(client)
}
