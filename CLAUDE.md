# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`safenet-monitor` is a Rust service that watches a Safenet consensus contract on an EVM-compatible blockchain and tracks metrics (e.g., transactions expected to be attested but weren't, validator stake status). It exposes Prometheus metrics over HTTP via Axum.

## Common Commands

```bash
cargo build             # Build debug binary
cargo run               # Run the service
cargo test              # Run all tests
cargo check             # Fast compilation check
cargo fmt               # Format code
cargo clippy            # Lint code
```

## Architecture

The service is built on:
- **`tokio`** — async multi-threaded runtime
- **`axum`** — HTTP server (metrics endpoint, health checks)
- **`alloy`** — Ethereum/EVM blockchain interaction (contract event watching, RPC calls)
- **`prometheus`** — metrics collection and exposition
- **`clap`** — CLI configuration (with `env` feature, so config can come from env vars)
- **`tracing` + `tracing-subscriber`** — structured JSON logging

The intended flow is: connect to a EVM RPCs (one on the staking chain and one on the consensus chain), subscribe to Safenet consensus contract transaction proposal and attestation events, query the validators' current stake amount, update Prometheus metrics, and serve those metrics over HTTP.

## Transaction Checks

`src/packets/checks/` implements the Safenet transaction policy — a predicate over `IConsensus::SafeTransaction` that decides whether a proposed transaction is valid. The public entry point is:

```rust
pub fn check_transaction(tx: &SafeTransaction) -> bool
```

The policy allows:
- **Calls** (`operation = 0`) to any non-self address.
- **Self-calls** (`operation = 0`, `to = safe`) to a whitelist of Safe management functions (`addOwnerWithThreshold`, `removeOwner`, `swapOwner`, `changeThreshold`, `disableModule`, and address-validated `setFallbackHandler`, `setGuard`, `enableModule`, `setModuleGuard`).
- **Delegate calls** (`operation = 1`) to known Safe migration contracts and signing libraries.
- **Multi-send** delegate calls to known `MultiSend` contract addresses, where every packed sub-transaction must itself pass the policy. The `0x218...` and `0xA83...` variants use the v1.5.0+ encoding (zero address in `to` means self-call).

Local `sol!` bindings that are only needed within a module are wrapped in a private `mod bindings { alloy::sol! { ... } }` to avoid polluting the module namespace. Reference them as `bindings::someFunctionCall::SELECTOR` etc. Import `use alloy::sol_types::SolCall as _;` to bring `abi_decode` and `abi_encode` into scope.

## Deployment

A multi-stage `Dockerfile` builds a release binary in `rust:1-slim` and produces a minimal `debian:13-slim` image with `tini` as the init process (PID 1 signal forwarding and zombie reaping).

GitHub Actions workflows live in `.github/workflows/`:
- **`ci.yml`** — runs on PRs and pushes to `main`: `cargo fmt --all --check`, `cargo clippy -- -D warnings`, `cargo test`.
- **`docker.yml`** — builds and pushes to `ghcr.io/safe-fndn/safenet-monitor` on pushes to `main` and version tags; builds (but does not push) on PRs.

## Multicall

In order to save on RPC credits, we try to batch calls with the `alloy` multicall feature.

``` rust
let mut multicall = provider.multicall().dynamic();

// For a list of validators, we can use `fold`.
let multicall = validators.fold(
    provider.multicall().dynamic(),
    |multicall, validator| {
        multicall.add_call_dynamic(
            some_call()
            // optionally allow failures...
                .allow_failure(true)
        )
    }
)

// Now we can process the result, and check for failures for the sub-calls:
multicall.aggregate3().await?;
```
