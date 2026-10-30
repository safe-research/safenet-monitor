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
    ├── mod.rs                  # ConsensusDomain (EIP-712), TransactionProposal
    └── checks/
        ├── mod.rs              # check_transaction(): static charter rule checks
        └── multi_send.rs       # Multi-send decoding (legacy & v1.5.0+)
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
| `oracle_timeout`                   | `24`                | Blocks validators wait for an oracle outcome            |
| `transaction_attestation_duration` | `48`                | Blocks after which an unattested proposal is recorded   |
| `validators`                       | `[]`                | Array of `{ name = "…", address = "0x…" }`              |

CLI arguments (via `argh`):

- `--config <path>` — path to TOML config (default: `config.toml`)
- `--log-filter <filter>` — tracing directives (default: `info`)

## Monitor Subsystems

All monitors live in `src/monitor/` and follow the same pattern: a struct with a `new(…, registry: &prometheus::Registry)` constructor that registers metrics, and an `async fn update(&mut self) -> Result<()>` called each scrape cycle.

### TransactionAttestations (`transactions.rs`)

Watches for `TransactionProposed` events on the consensus contract and `OracleResult`, `RequestTimedOut`, `DisputeTriggered`, `DisputeResolved`, `DisputeOutOfScope` and `ArbitrationTimedOut` events on the configured oracles (fetched together in a single `eth_getLogs` call). For each proposal it:

1. Decodes the event and ignores it if its `oracle` is not one of the configured `oracles` (validators ignore those too). Otherwise it runs the static charter checks (see below) on the Safe transaction and computes the EIP-712 `TransactionProposal(uint64 epoch,address oracle,bytes oracleData,bytes32 safeTxHash)` message, which is also the oracle request ID.
2. Records the oracle outcome from a matching event (keyed by `requestId = message`) emitted by the proposal's oracle: `OracleResult` → `approved`/`rejected`, and for the SentinelOracle, `RequestTimedOut` → `timeout` (nobody revealed) and `DisputeTriggered` → `disputed` (votes on both sides, requires arbitration). Any outcome in a block ≥ proposal block + `oracle_timeout` is ignored: validators process block timeouts before that block's events, so from their perspective the oracle request was never finalized.
3. Waits until the safe block reaches the proposal block + `attestation_duration`, then calls `getAttestationSignatureId()` (via multicall) to check if it was attested.
4. Increments a counter with the outcome. Validators only attest oracle-approved transactions, so `approved`+`unattested` logs a warning and any other outcome +`attested` logs an error.

If an `OracleResult` approves a proposal that failed the static charter checks, it logs an error and increments `oracle_rule_violations` (regardless of the oracle timeout, since it indicates misbehaving sentinels).

Additionally, a `DisputeTriggered` event (SentinelOracle) logs at `info` level with the request ID (the proposal message hash), since a disputed request requires arbitration and won't produce an `OracleResult` in time to be attested.

**Metrics**:

- `safenet_monitor_transactions_total{status, result}` — counter; `status` ∈ {`approved`, `rejected`, `timeout`, `disputed`, `unfinalized`} (oracle outcome, `unfinalized` if the oracle request was not finalized before the oracle timeout), `result` ∈ {`attested`, `unattested`}. All ten combinations are pre-initialized to 0. Arbitration of disputed requests happens long after validators give up, so it does not affect this metric (see `disputes` below).
- `safenet_monitor_disputes{status}` — counter; dispute lifecycle events on the configured oracles, independent of tracked proposals (arbitration usually happens long after a proposal is resolved). `status` ∈ {`triggered` (`DisputeTriggered`), `approved`/`denied` (`DisputeResolved` in favour of approval — a false positive — or denial — the dispute protected the user), `out_of_scope` (`DisputeOutOfScope`), `timeout` (`ArbitrationTimedOut`)}. All statuses are pre-initialized to 0. Resolutions log at `info` (with the arbitrator's `context`), arbitration timeouts at `warn`.
- `safenet_monitor_oracle_rule_violations{rule}` — counter; oracle approvals of proposals that obviously violate a deterministic charter rule (one increment per violated rule). `rule` ∈ {`settings_change` (R-4.1), `delegatecall` (R-4.2), `multi_send_depth` (monitor specific, see below)}. All rules are pre-initialized to 0. Meant for alerting.
- `safenet_monitor_transactions_last_processed_block` — int gauge; last safe block processed.

**Static charter checks** (`packets/checks/`): deliberately not exhaustive — only flag what is obviously insecure from the calldata alone, per the deterministic rules of the [Safenet Arbitration Charter](https://github.com/safe-research/safenet-charter/blob/latest-safedao-approved/Safenet_Arbitration_Charter.md) (Article IV, Part A):

- **R-4.1** `settings_change`: a call to the Safe itself with a settings-change selector (owners, threshold, modules, guards, fallback handler, `changeMasterCopy`), top-level or inside a delegatecalled multi-send. The only exception is a non-batched, zero-value top-level call with canonical ABI encoding to `disableModule` or `setFallbackHandler(address(0))`.
- **R-4.2** `delegatecall`: a delegatecall (top-level or inside a delegatecalled multi-send) to anything other than the canonical SignMessageLib, CreateCall or MultiSend contracts (v1.3.0, v1.4.1, v1.5.0). Migration contracts are not trusted (they change the singleton). Delegatecalled multi-sends are decoded and their sub-transactions checked recursively; delegatecalls inside call-only multi-sends are ignored since they revert. Multi-sends are decoded the way the contracts execute them: entries starting in the last 32 bytes are skipped and truncated entries are zero-padded (so trailing or truncated bytes cannot hide a violation); only batches that always revert (invalid operation, data length above 16 MiB) are not flagged.
- `multi_send_depth` (not a charter rule): a batch of more than 4 nested delegatecalled multi-sends (counting the top-level one). These are not inspected (the calldata is attacker controlled and each level copies the inner batch), so they are flagged as they may hide violations.

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
