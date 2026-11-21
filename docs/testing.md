# Testing

```bash
cargo test                 # everything (151 tests)
cargo test -p protocol-core
cargo test -p protocol-cli --test cli      # end-to-end CLI tests
```

No network is needed once dependencies are fetched (`cargo test --offline` works). The only third-party
dependency is `sha2` (ADR 0009).

## Layers

| Layer | Where | Covers |
|---|---|---|
| Unit | `crates/core/src/*` | amounts (fixed point), kv files, SHA-256 against NIST vectors, manifest parsing and rejection, risk rules, engine modes, supervisor lifecycle, store persistence, process runner (timeout, env cleared, output cap, strict parser) |
| Adapter | `crates/adapter-aave` | determinism, APY band, calm/stress health factor, protocol withdrawal rule, simulate-doesn't-mutate |
| Strategies | `crates/strategies` | each official strategy's decisions and permission manifests |
| End-to-end | `crates/cli/tests/cli.rs` | real CLI commands: approval flow, manual actions, kill switch, network refusal, live pause/resume/stop, verification pinning lifecycle, shadowing protection, external strategies bound by their manifests, restart/failure marking, timeouts, state on disk, and **create -> build -> test -> simulate -> run for Rust, Python, C, C++ and Java using the real toolchains** |

The language tests skip themselves (with a message) when a toolchain is not installed.

Race and failure-mode tests worth knowing about (each was checked to **fail on the previous code**):
two `run`s started at the same instant (exactly one wins), a stale `running` status after a crash
(`ps`/`pause`/`stop`/`run`/`approve` behave correctly), and a timed-out strategy whose forked
background child must be dead afterwards (Linux, via `/proc`). The memory-limit test uses `python3`
and skips itself if it is missing. `ulimit` and process-group behaviour is unix only.

## What passing tests mean

They show the code behaves as specified. They do **not** show that any strategy is profitable or
safe, that the simulated market resembles Aave, or that the sandbox is adequate (it is a thin layer over
bubblewrap, not independently reviewed). Never describe a strategy as safe or profitable because it passes `protocol test`.

## Not yet tested (because not yet built)

Real adapters against a node, signing, runtime isolation against hostile code (only accident-level
limits exist: see `docs/security.md`),
long-running soak tests, fuzzing of the line-protocol parser (recommended next), Windows.
Platform: developed and tested on Linux; `sh`-based build commands mean Windows is unsupported.

## Robustness and property tests

`crates/core/tests/robustness.rs` runs on every `cargo test` using only std: mutated and random
inputs for the strategy-output, manifest, action-line and amount parsers (no panics, accepted
inputs satisfy invariants), and property tests for balance conservation, risk `Allow` implying
every limit, and engine invariants over random action sequences (exposure cap, mode, network,
kill switch, health-factor floor). Coverage-guided fuzzing lives in `fuzz/` (see its README).
Scope for reviewers: `docs/audit-scope.md`.
