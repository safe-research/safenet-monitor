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
