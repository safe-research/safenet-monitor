mod bindings;
mod monitor;
mod packets;

use std::{io::IsTerminal as _, net::SocketAddr, path::PathBuf, sync::Arc};

use alloy::primitives::Address;
use argh::FromArgs;
use axum::{Router, extract::State, routing::get};
use serde::Deserialize;
use tower_http::trace::TraceLayer;

use monitor::{Monitor, Validator};

/// Monitor the Safenet consensus contract and expose Prometheus metrics.
#[derive(FromArgs)]
struct Args {
    /// path to the TOML configuration file
    #[argh(option, default = r#""config.toml".into()"#)]
    config: PathBuf,

    /// tracing log filter directives (default: info)
    #[argh(option, default = r#""info".to_owned()"#)]
    log_filter: String,
}

#[derive(Deserialize)]
struct Config {
    /// Socket address to bind the Prometheus metrics HTTP server to.
    #[serde(default = "Config::default_metrics_address")]
    metrics_address: SocketAddr,
    /// RPC URL for the consensus chain.
    consensus_rpc: String,
    /// RPC URL for the staking chain.
    staking_rpc: String,
    /// Address of the Safenet consensus contract on the consensus chain.
    #[serde(default = "Config::default_consensus_contract")]
    consensus_contract: Address,
    /// Address of the Safenet staking contract on the staking chain.
    #[serde(default = "Config::default_staking_contract")]
    staking_contract: Address,
    /// Address of the CumulativeMerkleDrop contract on the staking chain.
    #[serde(default = "Config::default_cumulative_merkle_drop")]
    cumulative_merkle_drop: Address,
    /// Validators to monitor.
    #[serde(default)]
    validators: Vec<Validator>,
    /// Maximum number of blocks within which a proposed transaction must be attested.
    #[serde(default = "Config::default_transaction_attestation_duration")]
    transaction_attestation_duration: u64,
}

impl Config {
    fn default_metrics_address() -> SocketAddr {
        "127.0.0.1:3777".parse().unwrap()
    }

    fn default_consensus_contract() -> Address {
        "0x223624cBF099e5a8f8cD5aF22aFa424a1d1acEE9"
            .parse()
            .unwrap()
    }

    fn default_staking_contract() -> Address {
        "0x115E78f160e1E3eF163B05C84562Fa16fA338509"
            .parse()
            .unwrap()
    }

    fn default_cumulative_merkle_drop() -> Address {
        "0xe5139Fc0FB8eae81e30d8a85C22E88c6757120f2"
            .parse()
            .unwrap()
    }

    fn default_transaction_attestation_duration() -> u64 {
        30
    }
}

#[tokio::main]
async fn main() {
    let args = argh::from_env::<Args>();

    init_tracing(&args.log_filter);

    let config: Config = {
        let content = std::fs::read_to_string(&args.config).expect("failed to read config file");
        toml::from_str(&content).expect("failed to parse config file")
    };

    tracing::info!(
        consensus_rpc = %config.consensus_rpc,
        staking_rpc = %config.staking_rpc,
        consensus_contract = %config.consensus_contract,
        staking_contract = %config.staking_contract,
        cumulative_merkle_drop = %config.cumulative_merkle_drop,
        "starting safenet-monitor",
    );

    let monitor = Arc::new(
        Monitor::new(
            config.consensus_rpc,
            config.staking_rpc,
            config.consensus_contract,
            config.staking_contract,
            config.cumulative_merkle_drop,
            config.validators,
            config.transaction_attestation_duration,
        )
        .await
        .expect("failed to initialize monitors"),
    );

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(monitor);

    let listener = tokio::net::TcpListener::bind(config.metrics_address)
        .await
        .expect("failed to bind metrics address");

    tracing::info!(address = %config.metrics_address, "metrics server listening");

    axum::serve(listener, app)
        .await
        .expect("metrics server error");
}

fn init_tracing(log_filter: &str) {
    let builder = tracing_subscriber::fmt().with_env_filter(log_filter);
    if std::io::stdout().is_terminal() {
        builder.init();
    } else {
        builder.json().init();
    }
}

async fn metrics_handler(State(monitor): State<Arc<Monitor>>) -> impl axum::response::IntoResponse {
    monitor.update().await;
    let (content_type, body) = monitor.encode();
    ([(axum::http::header::CONTENT_TYPE, content_type)], body)
}
