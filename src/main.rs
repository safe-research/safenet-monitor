mod bindings;
mod monitor;
mod packets;

use std::{io::IsTerminal as _, net::SocketAddr, sync::Arc};

use alloy::primitives::Address;
use argh::FromArgs;
use axum::{Router, extract::State, routing::get};
use tower_http::trace::TraceLayer;

use monitor::{Monitor, Validator};

/// Monitor the Safenet consensus contract and expose Prometheus metrics.
#[derive(FromArgs)]
struct Args {
    /// tracing log filter directives (default: info)
    #[argh(option, default = r#""info".to_owned()"#)]
    log_filter: String,

    /// address to bind the Prometheus metrics HTTP server to (default:
    /// 127.0.0.1:3777)
    #[argh(option, default = r#""127.0.0.1:3777".parse().unwrap()"#)]
    metrics_address: SocketAddr,

    /// consensus chain RPC URL
    #[argh(option)]
    consensus_rpc: String,

    /// staking chain RPC URL
    #[argh(option)]
    staking_rpc: String,

    /// safenet consensus contract address on the consensus chain (default:
    /// 0x223624cBF099e5a8f8cD5aF22aFa424a1d1acEE9)
    #[argh(
        option,
        default = r#""0x223624cBF099e5a8f8cD5aF22aFa424a1d1acEE9".parse().unwrap()"#
    )]
    consensus_contract: Address,

    /// safenet staking contract address on the staking chain (default:
    /// 0x115E78f160e1E3eF163B05C84562Fa16fA338509)
    #[argh(
        option,
        default = r#""0x115E78f160e1E3eF163B05C84562Fa16fA338509".parse().unwrap()"#
    )]
    staking_contract: Address,

    /// validator to monitor, in NAME@ADDRESS format (may be repeated)
    #[argh(option, arg_name = "NAME@ADDRESS")]
    validators: Vec<Validator>,

    /// maximum number of blocks it takes before a transaction is attested
    /// (default: 30)
    #[argh(option, default = "30")]
    transaction_attestation_duration: u64,
}

#[tokio::main]
async fn main() {
    let args = argh::from_env::<Args>();

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
        .await
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
