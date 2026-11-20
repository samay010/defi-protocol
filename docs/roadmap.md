# Roadmap

Mapped to the brief's phases. "Done" means implemented and tested in this release.

## Phase 1: Foundation: done (v0.1)
Rust workspace, architecture docs and ADRs, strategy interfaces (trait + line protocol), CLI.

## Phase 2: Aave integration: partly done
- Done: adapter interface, simulated Aave adapter, read-only (alert-only) strategies.
- Next: real read-only data via an Ethereum RPC (decide HTTP client and ABI approach in a new
  ADR first; dependencies must be justified), account/market queries, Aave rate and health-factor
  formulas checked against the real protocol. Testnet only.

## Phase 3: Strategy engine: mostly done
- Done: lifecycle, state persistence, scheduled continuous runs, pause/resume/stop, logging,
  error handling, restart.
- Next: event-driven triggers (new block, price move), structured logs / metrics, a long-running
  daemon so `run` can be detached (today it runs in the foreground; use a terminal multiplexer).

## Phase 4: Multi-language SDK: started
- Done: scaffolds + build + run for Rust, C, C++, Python, Java on one shared contract.
- Next: published SDK packages with typed helpers per language, protocol versioning, a
  conformance test-suite each SDK must pass, WASM/WASI runtime for Rust/C/C++, Go and TS later.

## Phase 5: Testing infrastructure: started
- Done: calm/stress simulation, `protocol test`.
- Next: backtesting on historical data, real transaction simulation (fork or `eth_call`),
  runtime isolation (OS sandbox, memory limits), parser fuzzing.

## Gate before any real-asset execution
All required: external security review of the sandbox and signer, independent audit, legal
and operational assessment, testnet period, and user-controlled signing in a separate process.

## Phase 6: Decentralized execution: research only
The off-chain-first decision (ADR 0002) means each user runs their own node, which already
removes any central operator. Independent keeper nodes, on-chain permissions (session keys / a
Safe module) and operator incentives are optional future research, justified only by a concrete
need (for example always-on execution without the user's machine). No token or DAO without a
clearly justified requirement.

## Phase 7: Community ecosystem: later
Public strategy library, community contributions, marketplace, dashboard (same engine as the
CLI, never a custodian), community governance, signed multi-reviewer verification.
