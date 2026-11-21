# DeFi Strategy Execution Protocol (v0.1, simulation-only)

An open-source, local-first engine for writing, testing and running automated DeFi strategies.
The core is Rust. Strategies can be written in **Rust, C, C++, Python or Java** and all speak
the same small contract. Every action a strategy proposes passes an independent risk engine
and your chosen execution mode before anything happens.

> **Status: simulation-only.** This release has a *simulated* Aave adapter with synthetic data.
> It does **not** connect to any chain, hold keys, sign, or move real assets. Testnet and mainnet
> execution are deliberately unavailable until security reviews, independent audits and legal and
> operational assessments are done (brief section 15). Nothing here is financial advice, and no
> strategy, test result or simulation shows that a strategy is profitable or safe.

## Why there is no smart contract

The engine runs on the user's own machine, strategies are code the user chooses to run, and
nothing is collected from users, so no on-chain component is needed (see
[ADR 0002](docs/adr/0002-off-chain-first.md)). Aave is called through its own contracts.

## Quick start

```bash
cargo build --release            # needs Rust 1.89+; one dependency (sha2), see ADR 0009
export PATH="$PWD/target/release:$PATH"

protocol strategies list
protocol strategies info stable-lending
protocol simulate health-factor-monitor --scenario stress --ticks 13

# run continuously with human approval for every action
protocol run stable-lending --mode approval --ticks 5 --interval-ms 200
protocol pending stable-lending
protocol approve stable-lending 1
protocol inspect stable-lending
```

Write your own strategy in any supported language:

```bash
protocol create my-strategy --lang python        # alert-only scaffold in ./custom-strategies
protocol build my-strategy
protocol test my-strategy
protocol simulate my-strategy
protocol verify my-strategy                      # prints the content hash; --pin after review
protocol run my-strategy --mode approval
```

State is stored in `~/.protocol` (override with `--home DIR` or `$PROTOCOL_HOME`).

## What is implemented (v0.1)

| Area | Status |
|---|---|
| Rust workspace, `protocol` CLI | implemented |
| Strategy contract, language-neutral line protocol | implemented |
| Out-of-process strategies: Rust, C, C++, Python, Java templates | implemented (build + run tested for all five) |
| Manifest with declared, least-privilege permissions | implemented |
| Risk engine (protocol/asset/action allow-lists, per-action and total caps, health-factor floor, kill switch) | implemented |
| Autonomous / Approval / Manual modes, approve/reject, manual actions | implemented |
| Supervisor: continuous run, pause/resume/stop, restart on failure, failure isolation | implemented |
| Persistent state, audit log | implemented (plain files) |
| Verification pins (content hash of reviewed source) | implemented |
| Official strategies: stable-lending, health-factor-monitor, rate-monitor | implemented (simulation) |
| Simulated Aave adapter | implemented (synthetic data) |
| Real read-only Aave data | **proposed** (Phase 2) |
| OS-level sandbox for non-Rust strategies, WASM runtime | **proposed** (Phase 4/5) |
| Real transaction simulation, signing, wallets, testnet | **proposed** |
| Backtesting, dashboard, marketplace, decentralized execution | **proposed** |

See [docs/roadmap.md](docs/roadmap.md).

## Important limits

- External strategy processes get a timeout (the whole process group is killed), a CPU-time limit,
  an optional memory limit, an output cap and an **empty environment**, but they are
  **not OS-sandboxed**: a malicious one could read your files or use the network as your user.
  Read the source of any strategy before running it. See [docs/security.md](docs/security.md).
- "Verified" means *reviewed*, not safe or profitable.

## Documentation

- [Architecture](docs/architecture.md)
- [Strategy protocol and SDK guide](docs/strategy-protocol.md) (all five languages)
- [CLI reference](docs/cli.md)
- [Security model](docs/security.md)
- [Testing](docs/testing.md)
- [Roadmap](docs/roadmap.md)
- [Governance, licensing and attribution](docs/governance-and-licensing.md)
- [Architecture decision records](docs/adr/)

## Tests

```bash
cargo test            # 138 tests: unit + end-to-end CLI, including real toolchains
```
