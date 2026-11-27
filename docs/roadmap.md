# Roadmap

Mapped to the brief's phases. "Done" means implemented and tested in this release.

## Phase 1: Foundation: done (v0.1)
Rust workspace, architecture docs and ADRs, strategy interfaces (trait + line protocol), CLI.

## Phase 2: Aave integration: partly done
- Done: adapter interface, simulated Aave adapter, read-only (alert-only) strategies.
- Done: read-only ABI layer for `Pool.getUserAccountData` (ADR 0013), read-only alloy transport,
  `protocol aave account` and `protocol watch` (live Sepolia data, aggregate USD view, suggestions only).
- Done: adapter abstraction (`protocol-chain`, `ReadOnlyAdapter`) and a read-only Compound III reader
  (`protocol compound account`); verified against Comet source and a scripted node, not a live market
  (no official Sepolia deployment found). Done: read-only Morpho Blue reader (`protocol morpho account`). Done: `watch --protocol aave|compound|morpho` (block/interval triggers for all, events for Aave). Done: `protocol backtest` (CSV replay, record-only: what a strategy would have proposed, not simulated P&L). Done: WASM isolation. Done: `protocol dashboard` (read-only localhost page, ADR 0016).
- Done: `protocol aave preflight` (real transaction simulation via `eth_call` from the account,
  Sepolia, no keys; revert reasons shown). Next: simulate strategy proposals through it, and
  `eth_call` state overrides or a fork for 'what if I had the funds'.
- Done: event-driven `watch` (`--on block|event`: new blocks, or Pool events touching the account;
  HTTP polling, read-only). Next: feed events to strategies (needs a line-protocol addition).
- Next: HTTPS JSON-RPC transport behind `EthCall` (audited crates, own dependency review), an
  official testnet Pool address supplied by the user, matching a real testnet position against the Aave UI (health-factor and base-currency scales
  are already confirmed from source), then market queries, Aave rate and health-factor
  formulas checked against the real protocol. Testnet only.

## Phase 3: Strategy engine: mostly done
- Done: lifecycle, state persistence, scheduled continuous runs, pause/resume/stop, logging,
  error handling, restart.
- Next: event-driven triggers (new block, price move), structured logs / metrics, a long-running
  daemon so `run` can be detached (today it runs in the foreground; use a terminal multiplexer).

## Phase 4: Multi-language SDK: started
- Done: scaffolds + build + run for Rust, C and C++ on one shared contract (Python and Java were removed on 2026-11-26, see ADR 0014).
- Next: published SDK packages with typed helpers per language, protocol versioning, a
  conformance test-suite each SDK must pass, Go and TS later.
- Done: WebAssembly strategies (`--lang wasm`): Rust built for `wasm32-wasip1`, run inside the
  in-process `wasmi` interpreter with a minimal WASI subset, fuel and memory limits (ADR 0015).
  Not yet independently reviewed; C/C++ to wasm is possible but has no template.

## Phase 5: Testing infrastructure: started
- Done: calm/stress simulation, `protocol test`.
- Next: backtesting on historical data, real transaction simulation (fork or `eth_call`),
  sandbox on macOS (WASM strategies already run there). Done: sandboxed `build`, seccomp deny-list, parser fuzzing
  (std-only tests; cargo-fuzz targets exist), process-group kill, CPU limit, opt-in memory limit, and
  the bubblewrap OS sandbox for external strategies on Linux (ADR 0012).

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
