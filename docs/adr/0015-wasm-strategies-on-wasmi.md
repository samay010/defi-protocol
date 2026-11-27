# 15. WebAssembly strategies on the `wasmi` interpreter

Status: accepted (2026-11-27); not yet independently reviewed

## Context
Strategies run as OS processes inside bubblewrap + seccomp (ADR 0012). That needs Linux and a
working `bwrap`, and it leaves a large kernel surface. After ADR 0014 every supported language
(Rust, C, C++) can compile to WebAssembly, which can isolate a strategy by construction.

## Decision
- New language `wasm` (Rust built for `wasm32-wasip1`). The manifest `entry` is a relative path to
  the `.wasm` file inside the strategy directory (validated: no `..`, no absolute path, no spaces).
- New crate `protocol-wasm` is the only place that depends on a WebAssembly runtime. The core
  crate stays free of it. Runtime: `wasmi` (a pure-Rust interpreter), chosen over `wasmtime`
  because it is far smaller to build (this project is built on a phone), has no JIT and no
  generated machine code, and per-tick work is tiny. Speed is not a goal.
- The host implements a minimal subset of `wasi_snapshot_preview1` itself instead of using a
  general WASI crate, so the capabilities are exactly the list in `crates/wasm/src/lib.rs`: stdin
  (fd 0), stdout and stderr (fds 1, 2; 1 MiB and 64 KiB caps), empty args and environment, a fixed
  pseudo-random stream, no clock (`ENOSYS`), `proc_exit`. Anything else is an unresolved import,
  so the module fails to start (fail closed).
- Per tick: a fresh instance, CPU limited by fuel (`tick_timeout_ms * 20000` instructions), memory
  limited by `max_memory_mb` (default 64 MiB). State lives in the engine, as for processes.
- The strategy reads the same line protocol on stdin and answers on stdout; the same strict parser
  is used (`DONE` mandatory).
- No bubblewrap is used for wasm strategies, so they also run where the OS sandbox does not
  exist (macOS) and `--sandbox require` does not refuse them.

## Consequences
- Stronger isolation of the guest than a process sandbox, with a much smaller trusted surface to
  review (`wasmi` + one file), but the host process itself is not sandboxed: a bug in `wasmi` or in
  the host functions would run with the user's rights. Neither has been independently audited.
- A new third-party dependency tree (`wasmi` and its crates). `cargo audit` / `cargo deny` cover it
  in CI; the licence allow-list gained `Apache-2.0 WITH LLVM-exception`.
- The fuel figure is an approximation, not a wall-clock timeout.
- The `.wasm` build output is not covered by the verification hash (like any build output): review
  and pin the source, build it yourself.
- Tests: the runner is tested against small hand-built modules (sources in
  `crates/wasm/fixtures-src/`, binaries in `crates/wasm/tests/fixtures/`) for the normal path, an
  endless loop, memory growth, unavailable file descriptors, non-zero exit, forbidden imports and
  output flooding. A full scaffold-build-run cycle runs in CI where the `wasm32-wasip1` target is
  installed.
- Not done: C/C++ templates for wasm, a wall-clock watchdog, WASI clocks, per-strategy files.
