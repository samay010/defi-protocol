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
   (manifest syntax), and the external `bubblewrap` binary (isolation).

## Modules by sensitivity

| Priority | Module | Why |
|---|---|---|
| Critical | `risk.rs`, `engine.rs` | deny-by-default checks, exposure caps, mode and kill-switch rules, re-check at approval |
| Critical | `external.rs`, `sandbox.rs`, `manifest.rs` | parse untrusted input; spawn processes; isolation; limits |
| High | `store.rs`, `supervisor.rs` | locking, kill switch, command inbox, restart logic |
| High | `hash.rs` + `sha2` | what "verified" is bound to |
| Medium | `adapter.rs`, adapter-aave | balance math, health factor |
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
  is not sandboxed. A reviewer should check the exposed paths in `bwrap_args` and the fail-closed
  logic in `prepare`.
- Unix only.
- The Aave adapter is a simulation with synthetic data. `adapter-aave/src/onchain.rs` is a
  read-only ABI layer (no I/O, no keys, cannot sign or send); scales are still unconfirmed.
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
