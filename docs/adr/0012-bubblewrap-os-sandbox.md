# ADR 0012: OS sandbox with bubblewrap

Status: accepted

## Context
Strategies are untrusted code in five languages, run on the user's own machine. Until now they
ran as ordinary child processes: limits for accidents, but a malicious strategy could read the
user's files or use the network. WASM would isolate Rust/C/C++ but not Python or Java, which
the project wants to support.

## Decision
Run external strategy processes inside `bubblewrap` (`bwrap`), the unprivileged namespace
sandbox used by Flatpak. We build its command line (`crates/core/src/sandbox.rs`) and do not
implement isolation ourselves, and we add no Rust crate. The sandbox has no network, a read-only
view of system directories and of the strategy's own directory, a private `/tmp`, and its own
PID/IPC/UTS/user namespaces, and it dies with the supervisor. Only a few `/etc` entries are
exposed (loader cache, alternatives, Java config), never all of `/etc`.

Policy is fail closed and on by default: `--sandbox require`. If `bwrap` is missing or cannot
start, the strategy does not run and the error says how to fix it. `--sandbox off` is an explicit,
loudly-announced opt-out. The strategy cannot loosen it: the flag is the user's, not the manifest's.

## Consequences
- Needs `bubblewrap` installed (`sudo apt install bubblewrap`) and user namespaces enabled.
- Linux only. On macOS and Windows users must pass `--sandbox off`.
- `protocol build` stays unsandboxed (compilers need network and write access): documented.
- The strategy directory is read-only, so a strategy cannot write beside itself; `/tmp` is writable but private and discarded.
- No seccomp syscall filter yet; a kernel or `bwrap` bug would defeat the sandbox.
- Isolation tests run only where `bwrap` works and are skipped elsewhere (CI installs it).
