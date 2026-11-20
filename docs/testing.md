# Testing

```bash
cargo test                 # everything (125 tests)
cargo test -p protocol-core
cargo test -p protocol-cli --test cli      # end-to-end CLI tests
```

No network is needed; the project has no third-party dependencies.

## Layers

| Layer | Where | Covers |
|---|---|---|
| Unit | `crates/core/src/*` | amounts (fixed point), kv files, SHA-256 against NIST vectors, manifest parsing and rejection, risk rules, engine modes, supervisor lifecycle, store persistence, process runner (timeout, env cleared, output cap, strict parser) |
| Adapter | `crates/adapter-aave` | determinism, APY band, calm/stress health factor, protocol withdrawal rule, simulate-doesn't-mutate |
| Strategies | `crates/strategies` | each official strategy's decisions and permission manifests |
| End-to-end | `crates/cli/tests/cli.rs` | real CLI commands: approval flow, manual actions, kill switch, network refusal, live pause/resume/stop, verification pinning lifecycle, shadowing protection, external strategies bound by their manifests, restart/failure marking, timeouts, state on disk, and **create -> build -> test -> simulate -> run for Rust, Python, C, C++ and Java using the real toolchains** |

The language tests skip themselves (with a message) when a toolchain is not installed.

## What passing tests mean

They show the code behaves as specified. They do **not** show that any strategy is profitable or
safe, that the simulated market resembles Aave, or that the sandbox is adequate (there isn't one
yet). Never describe a strategy as safe or profitable because it passes `protocol test`.

## Not yet tested (because not yet built)

Real adapters against a node, signing, runtime isolation against hostile code, memory limits,
long-running soak tests, fuzzing of the line-protocol parser (recommended next), Windows.
Platform: developed and tested on Linux; `sh`-based build commands mean Windows is unsupported.
