# Audit scope

What an independent reviewer should look at, in priority order. Nothing here claims the code
has been audited; it has not. Funds stay simulation-only until audits are done.

## Trust boundaries

1. **Strategy -> engine.** A strategy is untrusted. Its only channel is the line protocol
   (`external::parse_output`). Everything it proposes passes the risk engine.
2. **Manifest -> policy.** `strategy.toml` is untrusted input and defines the permissions
   (`manifest::Manifest::parse`, `policy()`).
3. **Engine -> adapter.** Only the engine calls `execute`, and only after `risk::check`.
4. **Disk state.** Files under the data directory are trusted only as far as the user owns them.
   Atomic writes; the lock file is the truth for "running".
5. **Third-party dependencies:** `sha2` (hashing for pinning), `clap` (arguments), `serde` + `toml`
   (manifest syntax), `libc` + `seccompiler` (syscall deny-list, Linux only), `alloy` + `tokio`
   (read-only RPC, `crates/rpc` only), and the external `bubblewrap` binary (isolation).

## Modules by sensitivity

| Priority | Module | Why |
|---|---|---|
| Critical | `risk.rs`, `engine.rs` | deny-by-default checks, exposure caps, mode and kill-switch rules, re-check at approval |
| Critical | `external.rs`, `sandbox.rs`, `seccomp.rs`, `manifest.rs` | parse untrusted input; spawn processes; isolation (files, network, syscalls); limits |
| High | `store.rs`, `supervisor.rs` | locking, kill switch, command inbox, restart logic |
| High | `hash.rs` + `sha2` | what "verified" is bound to |
| Medium | `adapter.rs`, adapter-aave, adapter-compound, adapter-morpho, chain | balance math, health factor |
| Low | `amount.rs`, `kv.rs`, `types.rs`, CLI | parsing helpers and presentation |

## Existing evidence

- Unit and end-to-end tests, plus `crates/core/tests/robustness.rs` (mutation fuzz-lite and
  property tests, seeded and reproducible). Its first run found two real input-validation gaps
  (`DONE <garbage>` accepted; control characters accepted in asset/protocol of an action line);
  both are fixed and covered.
- Coverage-guided targets in `fuzz/` (not yet run for long campaigns).
- CI: fmt, clippy `-D warnings`, tests, `cargo audit`, `cargo deny`.

## Known limitations (do not rely on these)

- Process limits are accident containment. Isolation comes from `sandbox.rs`, which only builds a
  `bubblewrap` command line; it is Linux-only and `--sandbox off` removes it. `protocol build`
  runs in a variant with the network open and the project directory writable, so a build can
  still use the network and modify its own source. The seccomp filter is a deny-list (a syscall
  not on it is allowed). macOS and Windows have no sandbox: the default refuses to run external
  strategies there. A reviewer should check the exposed paths in `bwrap_args` and
  `bwrap_build_args`, the `DENIED` list in `seccomp.rs`, and the fail-closed logic in `prepare`.
- Unix only.
- The Aave adapter is a simulation with synthetic data. `adapter-aave/src/onchain.rs` is a
  read-only ABI layer (no I/O, no keys, cannot sign or send); health factor (1e18) and base-currency (USD, 1e8) scales are confirmed from Aave's
  source/oracle; ltv/threshold bps are not yet matched against a real position.
- `crates/rpc` is the only network-facing code (alloy, tokio, reqwest/rustls). The dependency tree
  is large; see ADR 0013 for the accepted advisory and the no-key-holding-crate CI check.
- "Verified" means a named reviewer read that exact source tree. It does not mean safe or profitable.
- Health-factor math uses `f64`; fine for simulation, revisit with fixed-point before real funds.

## Questions for the auditor

- Can any path reach `Adapter::execute` without `risk::check`?
- Can a manifest widen permissions beyond what it declares, or a name escape the data directory?
- Can a strategy influence the exact bytes that get hashed or the files read?
- Are there races between lock, status and command inbox that allow two supervisors?

## Out of scope for now

Real on-chain interaction, key management (the project deliberately never handles keys),
network protocol code.

- Backtest (`core/src/replay.rs`): CSV parser with size limits (100000 rows, 8 MiB) and a replay adapter that serves row `tick - 1`; fund-moving actions are errors. A backtest reports what a strategy would have proposed; it does not model prices, interest or liquidation outcomes.

- WebAssembly host (`crates/wasm`): the WASI subset (`fd_read`/`fd_write` on fds 0-2 only, no file, socket or clock functions), the fuel and memory limits, output caps, and the use of the third-party `wasmi` interpreter. The host process is not OS-sandboxed (no child process exists), so a `wasmi` escape would run with the user's rights.

## Items a reviewer must confirm against contract source
- Morpho Blue reader (`adapter-morpho`): `SharesMathLib.toAssetsUp/Down` formulas (`mulDiv(shares, totalAssets + VIRTUAL_ASSETS, totalShares + VIRTUAL_SHARES)`), `VIRTUAL_SHARES = 1e6`, `VIRTUAL_ASSETS = 1` and `ORACLE_PRICE_SCALE = 1e36`. These were not read from `SharesMathLib.sol`/`ConstantsLib.sol` directly (unreachable from the build sandbox); the first two match Morpho's official SDK.
- Compound III reader: `isLiquidatable` rule was read from Comet source at commit `f766f51`; no live market has been tested.

## Dashboard
`crates/dashboard` (HTTP parsing, token and Host checks, headers, page) and `crates/cli/src/dash.rs` (read-only data provider).
