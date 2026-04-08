mod bindings;
mod monitor;
mod packets;

use std::{io::IsTerminal as _, net::SocketAddr, sync::Arc};

use alloy::primitives::Address;
use axum::{Router, extract::State, routing::get};
use clap::Parser;
use tower_http::trace::TraceLayer;

use monitor::{Monitor, Validator};

#[derive(Parser)]
#[command(about = "Monitor the Safenet consensus contract and expose Prometheus metrics")]
struct Args {
    /// Tracing log filter directives.
    #[arg(long, env = "LOG_FILTER", default_value = "info")]
    log_filter: String,

    /// Address to bind the Prometheus metrics HTTP server to.
    #[arg(long, env = "METRICS_ADDRESS", default_value = "127.0.0.1:3777")]
    metrics_address: SocketAddr,

    /// Consensus chain RPC URL.
    #[arg(long, env = "CONSENSUS_RPC_URL")]
    consensus_rpc: String,

    /// Staking chain RPC URL.
    #[arg(long, env = "STAKING_RPC_URL")]
    staking_rpc: String,

    /// Safenet consensus contract address on the consensus chain.
    #[arg(long, env = "CONSENSUS_CONTRACT")]
    consensus_contract: Address,

    /// Safenet staking contract address on the staking chain.
    #[arg(long, env = "STAKING_CONTRACT")]
    staking_contract: Address,

    /// Validator to monitor, in NAME@ADDRESS format (may be repeated).
    #[arg(long = "validator", value_name = "NAME@ADDRESS")]
    validators: Vec<Validator>,

    /// Maximum number of blocks it takes before a transaction is attested.
    #[arg(long, env = "TRANSACTION_ATTESTATION_DURATION", default_value_t = 30)]
    transaction_attestation_duration: u64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    init_tracing(&args.log_filter);

    tracing::info!(
        consensus_rpc = %args.consensus_rpc,
        staking_rpc = %args.staking_rpc,
        consensus_contract = %args.consensus_contract,
        staking_contract = %args.staking_contract,
        "starting safenet-monitor",
    );

    let monitor = Arc::new(
        Monitor::new(
            args.consensus_rpc,
            args.staking_rpc,
            args.consensus_contract,
            args.staking_contract,
            args.validators,
            args.transaction_attestation_duration,
        )
        .expect("failed to initialize monitors"),
    );

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(monitor);

    let listener = tokio::net::TcpListener::bind(args.metrics_address)
        .await
        .expect("failed to bind metrics address");

    tracing::info!(address = %args.metrics_address, "metrics server listening");

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
