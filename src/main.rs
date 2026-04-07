mod monitor;

use std::{io::IsTerminal as _, net::SocketAddr, time::Duration};

use alloy::primitives::Address;
use axum::{Router, routing::get};
use clap::Parser;
use prometheus::{Encoder, TextEncoder};
use tower_http::trace::TraceLayer;

#[derive(Parser)]
#[command(about = "Monitor the Safenet consensus contract and expose Prometheus metrics")]
struct Args {
    /// Tracing log filter directives.
    #[arg(long, env = "LOG_FILTER", default_value = "info,safenet_monitor=debug")]
    log_filter: String,

    /// Address to bind the Prometheus metrics HTTP server to.
    #[arg(long, env = "METRICS_ADDRESS", default_value = "127.0.0.1:3777")]
    metrics_address: SocketAddr,

    /// How often to poll for updates, in fractional seconds.
    #[arg(long, env = "UPDATE_PERIOD", default_value = "60", value_parser = parse_duration)]
    update_period: Duration,

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
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let secs: f64 = s.parse().map_err(|e| format!("{e}"))?;
    Ok(Duration::from_secs_f64(secs))
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
        update_period = %format_args!("{}s", args.update_period.as_secs_f64()),
        "starting safenet-monitor",
    );

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .layer(TraceLayer::new_for_http());

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

async fn metrics_handler() -> impl axum::response::IntoResponse {
    let encoder = TextEncoder::new();
    let families = prometheus::gather();
    let mut buffer = Vec::new();
    encoder.encode(&families, &mut buffer).unwrap();
    let content_type = encoder.format_type().to_owned();
    ([(axum::http::header::CONTENT_TYPE, content_type)], buffer)
}
