# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`safenet-monitor` is a Rust service that watches a Safenet (Aegis) consensus contract on an EVM-compatible blockchain and tracks metrics (e.g., oracle-approved transactions that weren't attested, validator stake status). It exposes Prometheus metrics over HTTP via Axum.

## Common Commands

```bash
cargo build             # Build debug binary
cargo run               # Run the service
cargo test              # Run all tests
cargo check             # Fast compilation check
cargo fmt               # Format code
cargo clippy            # Lint code
```

## File Structure

```
src/
├── main.rs                     # CLI args (argh), TOML config, Axum HTTP server
├── bindings.rs                 # ABI bindings: IConsensus, IOracle, ISentinelOracle, IStaking (alloy sol!)
├── monitor/
│   ├── mod.rs                  # Monitor struct, provider setup, update() + encode()
│   ├── transactions.rs         # TransactionAttestations monitor
│   ├── stake.rs                # ValidatorStake + TotalStake monitors
│   ├── balances.rs             # ValidatorBalances monitor
│   ├── airdrop.rs              # CumulativeMerkleDropBalance monitor
│   ├── gas.rs                  # BaseGasFee monitor
│   └── utils.rs                # approx_units(): U256 → f64 token amount
└── packets/
    └── mod.rs                  # ConsensusDomain (EIP-712), TransactionProposal
```

## Architecture

The service is built on:

- **`tokio`** — async multi-threaded runtime
- **`axum`** — HTTP server (`GET /metrics` endpoint)
- **`alloy`** — EVM RPC client, `sol!` ABI bindings, multicall batching
- **`prometheus`** — metrics collection and Prometheus text exposition
- **`argh`** — CLI argument parsing
- **`tracing` + `tracing-subscriber`** — structured logging (human format on TTY, JSON otherwise)

**Execution model**: updates are on-demand, not on a background timer. Each `GET /metrics` request calls `monitor.update()`, which runs all monitor subsystems concurrently via `tokio::join!()`. A `tokio::sync::Mutex` ensures concurrent requests don't trigger redundant updates — the second caller waits for the first to finish and returns the freshly updated metrics.

## Configuration

TOML file (default: `config.toml`), selected via `--config <path>`.

| Field                              | Default             | Description                                             |
| ---------------------------------- | ------------------- | ------------------------------------------------------- |
| `metrics_address`                  | `127.0.0.1:3777`    | Bind address for Prometheus HTTP server                 |
| `consensus_rpc`                    | _(required)_        | RPC URL for consensus chain (Gnosis)                    |
| `staking_rpc`                      | _(required)_        | RPC URL for staking chain (Ethereum mainnet)            |
| `consensus_contract`               | `0xc855...aa96`     | Safenet consensus contract address                      |
| `oracles`                          | `["0x4F61...1d03"]` | Oracle contracts honored by validators (SentinelOracle) |
| `staking_contract`                 | `0x115E...8509`     | Safenet staking contract address                        |
| `cumulative_merkle_drop`           | `0xe513...20f2`     | CumulativeMerkleDrop address on the staking chain       |
| `transaction_attestation_duration` | `30`                | Blocks after which an unattested proposal is recorded   |
| `validators`                       | `[]`                | Array of `{ name = "…", address = "0x…" }`              |

CLI arguments (via `argh`):

- `--config <path>` — path to TOML config (default: `config.toml`)
- `--log-filter <filter>` — tracing directives (default: `info`)

## Monitor Subsystems

All monitors live in `src/monitor/` and follow the same pattern: a struct with a `new(…, registry: &prometheus::Registry)` constructor that registers metrics, and an `async fn update(&mut self) -> Result<()>` called each scrape cycle.

### TransactionAttestations (`transactions.rs`)

Watches for `TransactionProposed` events on the consensus contract and `OracleResult` / `DisputeTriggered` events on the configured oracles (fetched together in a single `eth_getLogs` call). For each proposal it:

1. Decodes the event and ignores it if its `oracle` is not one of the configured `oracles` (validators ignore those too). Otherwise it computes the EIP-712 `TransactionProposal(uint64 epoch,address oracle,bytes oracleData,bytes32 safeTxHash)` message, which is also the oracle request ID.
2. Records the oracle verdict from a matching `OracleResult(requestId = message)` event emitted by the proposal's oracle.
3. Waits `attestation_duration` safe-blocks, then calls `getAttestationSignatureId()` (via multicall) to check if it was attested.
4. Increments a counter with the outcome. Validators only attest oracle-approved transactions, so `approved`+`unattested` logs a warning and `rejected`/`unresolved`+`attested` logs an error.

Additionally, a `DisputeTriggered` event (SentinelOracle) logs at `info` level with the request ID (the proposal message hash), since a disputed request requires arbitration and won't produce an `OracleResult` in time to be attested.

**Metrics**:

- `safenet_monitor_transactions_total{status, result}` — counter; `status` ∈ {`approved`, `rejected`, `unresolved`} (oracle verdict, `unresolved` if no result before the deadline), `result` ∈ {`attested`, `unattested`}. All six combinations are pre-initialized to 0.
- `safenet_monitor_transactions_last_processed_block` — int gauge; last safe block processed.

**State**: proposals are held in a `HashMap<B256 (EIP-712 message hash), Proposal>` between updates; removed once resolved.

### ValidatorStake (`stake.rs`)

Reads each validator's stake via two sequential multicall batches:

1. `IConsensus.getValidatorStaker(validator)` on the consensus chain → staker address.
2. `IStaking.stakes(staker, validator)` on the staking chain → stake `U256`.

**Metrics**:

- `safenet_monitor_validator_stake{validator}` — gauge; stake in token units (via `utils::approx_units()`).

### TotalStake (`stake.rs`)

Reads the aggregate stake on the staking contract via a single call to `IStaking.totalStakedAmount()`.

**Metrics**:

- `safenet_monitor_total_stake` — gauge; total stake in token units (via `utils::approx_units()`).

### CumulativeMerkleDropBalance (`airdrop.rs`)

Reads the rewards token address from the configured CumulativeMerkleDrop contract via `token()` once during monitor initialization, then reads `IERC20.balanceOf(cumulative_merkle_drop)` for that token on the staking chain during each update.

**Metrics**:

- `safenet_monitor_cumulative_merkle_drop_balance` — gauge; token balance held by the CumulativeMerkleDrop contract, in token units (via `utils::approx_units()`).

### ValidatorBalances (`balances.rs`)

Fetches each validator's native token balance on the consensus chain using `IMulticall3.getEthBalance()`.

**Metrics**:

- `safenet_monitor_validator_balance{validator}` — gauge; balance in token units.

### BaseGasFee (`gas.rs`)

Fetches the base gas fee on the consensus chain using `eth_feeHistory(1, "latest", [])` and reads the first element of `base_fee_per_gas`.

**Metrics**:

- `safenet_monitor_base_gas_fee` — gauge; base gas fee in Gwei.

## Adding a New Monitor

1. Create `src/monitor/<name>.rs` with a public struct, a `new(…, registry: &prometheus::Registry) -> Result<Self>` constructor (registers metrics there), and `pub async fn update(&mut self) -> Result<()>`.
2. Add the new struct as a field on `Inner` in `src/monitor/mod.rs`.
3. Instantiate it inside `Monitor::new()` and add it to the `tokio::join!()` in `Monitor::update()`.
4. Wire any new config fields through `Config` in `main.rs` and pass them into `Monitor::new()`.
5. Update this file to include information on the new monitor.

## Deployment

A multi-stage `Dockerfile` builds a release binary in `rust:1-slim` and produces a minimal `debian:13-slim` image with `tini` as the init process (PID 1 signal forwarding and zombie reaping).

GitHub Actions workflows live in `.github/workflows/`:

- **`ci.yml`** — runs on PRs and pushes to `main`: `cargo fmt --all --check`, `cargo clippy -- -D warnings`, `cargo test`.
- **`docker.yml`** — builds and pushes to `ghcr.io/safe-research/safenet-monitor` on pushes to `main` and version tags; builds (but does not push) on PRs.

## Multicall

In order to save on RPC credits, we try to batch calls with the `alloy` multicall feature.

```rust
// For a list of validators, use `fold` to build the batch.
let multicall = validators.iter().fold(
    provider.multicall().dynamic(),
    |multicall, validator| {
        multicall.add_call_dynamic(
            CallItem::<SomeCall>::new(contract, SomeCall { … }.abi_encode().into())
                .allow_failure(true),
        )
    },
);

// Execute and get one Result<T> per call.
let results = multicall.aggregate3().await?;
```

`CallItem` requires a type parameter for the return-type ABI decoding. Use `.allow_failure(true)` when a per-validator failure shouldn't abort the whole batch — the corresponding entry in `results` will be `Err(…)`.
