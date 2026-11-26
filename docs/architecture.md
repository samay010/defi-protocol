# Architecture

Status legend: **implemented** (in this release, tested) vs **proposed** (designed, not built).

## Data flow

```
 Strategy (any language)          plain data only, no keys, no signing
        |  proposals
        v
 +-------------+   +---------------+   +--------------------------+
 | Risk engine |-->| Dry-run in    |-->| Execution engine         |
 | (allow-lists|   | the adapter   |   |  autonomous: execute     |
 |  caps, HF,  |   | + risk again  |   |  approval:   queue       |
 |  kill switch)|  | on post-state |   |  manual:     suggest only|
 +-------------+   +---------------+   +--------------------------+
                                                  |
                                       Protocol adapter (simulated Aave)
```

Every action takes this path whether it came from a strategy or from the user (`protocol act`).
A user action skips the approval queue (the user *is* the approval) but never skips risk checks.
Queued actions are risk-checked **again** at approval time because state may have changed.

## Crates (Rust workspace)

| Crate | Responsibility |
|---|---|
| `protocol-core` | Everything language- and protocol-neutral (below) |
| `replay` (in `protocol-core`) | CSV history parser, stats and `ReplayAdapter` for `protocol backtest` (read-only, std only) |
| `protocol-chain` | Shared read-only chain types: `Address`, `EthCall` trait, hex and range guards (no keys, no signing) |
| `protocol-adapter-compound` | Read-only Compound III (Comet) reader: balances, health factor, liquidation rule (no I/O of its own) |
| `protocol-adapter-morpho` | Read-only Morpho Blue reader: one market, one account, 256-bit-safe health math (no I/O of its own) |
| `protocol-adapter-aave` | Simulated Aave adapter (deterministic synthetic markets, calm/stress scenarios) |
| `protocol-strategies` | Official built-in strategies with manifests and docs |
| `protocol-cli` | The `protocol` binary, per-language templates, argument handling |

### `protocol-core` modules vs the brief's section 8

| Brief module | Where | Status |
|---|---|---|
| Strategy Runtime | `runtime.rs` (one tick, panic/error containment), `external.rs` (process runner) | implemented |
| Strategy Supervisor | `supervisor.rs` (lifecycle, continuous loop, restart, failure marking) | implemented |
| Strategy SDK interfaces | `strategy.rs` (trait) + line protocol (`docs/strategy-protocol.md`) + CLI templates | implemented (templates, not published SDK packages) |
| Protocol Adapters | `adapter.rs` (trait, `Snapshot`, ledger logic) + `protocol-adapter-aave` | implemented (simulated) |
| Event Monitoring | tick-based scheduled evaluation only | **partial**: event-driven triggers are proposed |
| Execution Engine | `engine.rs` | implemented (simulation) |
| Risk Engine | `risk.rs` | implemented |
| State Management | `store.rs` (plain files, atomic writes) | implemented |
| Logging / Observability | append-only `audit.log` per instance, tick log lines | implemented (no metrics/tracing yet) |
| Configuration | `manifest.rs` (`strategy.toml`) + CLI flags | implemented |

## Strategy contract

A strategy is a function `(snapshot, params, previous state) -> (logs, proposals, new state)`.
Official strategies implement the Rust `Strategy` trait in-process. Anything else is a separate
process speaking a line protocol on stdin/stdout, spawned once per tick. The engine owns the
state, so a crashed strategy restarts with nothing lost. This is the *same contract* for all
languages, which is what lets a Python and a Rust developer use the same platform capabilities.

## Manifest permissions

`strategy.toml` declares protocols, assets, allowed action kinds, per-action cap, total exposure
cap and minimum health factor. Defaults are least-privilege: only `alert`, zero caps. Unknown
keys are errors so a typo cannot silently change permissions. The manifest is turned into a
`RiskPolicy`; the strategy cannot widen it at runtime.

## Control plane

Each instance is a directory under the home (`~/.protocol/instances/<name>/`):
`status` (own file so `pause`/`resume`/`stop` from another process never race with metadata
writes), `instance.state`, `account.state`, `pending.state`, `strategy.state`, `audit.log`, and a
`commands/` inbox. `approve`, `reject` and `act` drop a command into the inbox; a running
supervisor applies it between ticks (also while paused), and for a non-running instance the CLI
applies it immediately against the persisted state. The global kill switch is a `KILL` marker
file read before every tick.

**Who is running an instance** is decided by an OS file lock (`run.lock`), not by the `status`
file: `run` takes the lock for its whole lifetime, a second `run` is refused, and the OS releases
the lock if the process dies. A `status` of `running`/`paused` with no lock holder is shown as
`stale`; `run` simply continues from the saved state, `stop` clears the status, and `pause`/`resume`
refuse to pretend they control a dead run. The CLI decides between "send the command to the live
run" and "apply it now" by whether it can take the lock. See ADR 0008.

## Failure model

- Strategy error, timeout, bad output, panic: caught per tick, reported, strategy re-created
  (restart), instance marked `failed` after more than `--max-failures` consecutive failures.
- One instance failing never affects another instance or the CLI.
- A timed-out strategy is killed together with every process it spawned (process group).
- State is written atomically (write temp file, rename).

## Not built yet (proposed)

Other protocol adapters (Compound, Morpho), event-driven monitoring,
WASM/WASI runtime, a macOS sandbox, real transaction simulation,
user-controlled signing in a separate process, backtesting, dashboard, marketplace and
decentralized execution. See `docs/roadmap.md`.
