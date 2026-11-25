# Security model

Security is a foundation of this project, but this release is **not audited** and moves **no real
assets**. Real-asset execution stays disabled until security reviews, independent audits and legal
and operational assessments are complete.

## Trust boundaries

- **No server, no collection.** The project operates no service. Strategy code and state live on
  the user's machine; nothing is uploaded. There is nothing of the project's to leak.
- **Keys never enter strategies.** A strategy's output is plain data (`PROPOSE ...`). There is no
  signing function and no key in its input or its environment (it is started with an empty
  environment). Private keys must never be stored in strategy source. When real execution is
  built, signing lives in a **separate signer process** (OS keychain or hardware wallet) that the
  strategy process cannot reach. *(Signer: proposed. Nothing signs today.)*
- **Declared permissions, enforced outside the strategy.** The risk engine enforces the manifest
  no matter what language the strategy uses or what it proposes.

## What is implemented

| Control | Detail |
|---|---|
| Least-privilege manifest | alert-only and zero caps by default; unknown manifest keys rejected |
| Risk engine | protocol / asset / action allow-lists, per-action cap, cumulative exposure cap, health-factor floor on withdrawals, zero-amount rejection |
| Dry-run + re-check | adapter simulates the action, risk runs again on the resulting state |
| Execution modes | autonomous / approval / manual; approvals re-checked at approval time |
| Kill switch | `protocol kill-switch on` blocks all fund-moving actions, alerts still flow |
| Network gate | mainnet denied by risk; testnet not implemented; only simulation executes |
| Process limits | per-tick timeout (on expiry the strategy's whole process group is killed, so forked background children die too), CPU-time limit (`ulimit -t`, timeout rounded up + 1 s), optional address-space limit (`max_memory_mb`, `ulimit -v`, fails closed if it cannot be applied), 64 KiB output cap, 20 proposals/tick, empty environment |
| One run per instance | an OS file lock (`run.lock`) makes a second `run` of the same instance fail, even when both start at the same instant; the lock disappears when the process dies, so a crash leaves no stuck instance. `ps` shows a dead run as `stale` |
| Strict parsing | malformed or truncated strategy output fails the tick |
| Fault isolation | errors/panics caught per tick; restart, then mark failed; other instances unaffected |
| Audit log | append-only `audit.log` of every decision with the reason |
| Safe paths | instance and strategy names are restricted (no `/`, `..`); symlinks are not followed when hashing |
| Atomic state writes | temp file + rename |
| Verification pinning | name + version + SHA-256 of the source; any change un-verifies |

## What is NOT implemented (read this)

1. **The OS sandbox is Linux-only, depends on `bubblewrap`, and has not been independently
   reviewed.** By default (`--sandbox require`) an external strategy runs inside `bwrap`: no
   network, no view of your home or other files (read-only system dirs and its own read-only
   directory only), private `/tmp`, own PID/IPC/UTS/user namespaces, and a seccomp deny-list that refuses
   dangerous syscalls (ptrace, mount, bpf, io_uring, perf_event_open, keyrings, kernel modules,
   namespace changes, clock changes; see `crates/core/src/seccomp.rs`). The list is a deny-list, so a
   syscall not on it is still allowed: it shrinks the kernel attack surface, it is not a full policy. If `bwrap` is missing or the
   host forbids user namespaces, the strategy does **not** run (fail closed). `--sandbox off`
   runs it as an ordinary child process of your user, where a malicious strategy could read your
   files, use the network, or fork background processes; the CLI prints `sandbox: OFF` then.
   `protocol build` runs in the same kind of sandbox but keeps the network (compilers fetch
   dependencies) and may write to the project directory; it sees the Rust toolchain read-only and
   none of your other files. Because the whole project directory is writable during a build, a
   build script can modify the source: pinned strategies detect this through the verification
   hash. A build can still download and run arbitrary dependency code and use the network, so
   read the source and dependencies before building. Not covered: macOS/Windows (there is no
   sandbox there yet, so the default `--sandbox require` refuses to run external strategies and
   builds with a message saying so; `--sandbox off` runs them with your user's full access,
   knowingly; built-in strategies are unaffected), kernel or `bwrap` vulnerabilities. Strategies can
   still burn CPU within the limits and, through the line protocol, propose actions (which the
   risk engine checks). **Do not run strategies you have not read.**
2. **Resource limits are partial.** Memory is limited only if the manifest sets `max_memory_mb`
   (Linux only: on other systems such a manifest is refused instead of silently ignored)
   (address space, not resident memory; left off by default because the JVM reserves far more
   address space than it uses and will not start under a tight limit). There are no limits on
   open files, file size or number of processes, and the CPU limit is best effort (skipped if the
   user's own hard limit is already lower). The limits come from `ulimit` in a `/bin/sh` wrapper
   (unix only). They contain accidents; they are not a security boundary.
   Because the strategy runs in its own process group, Ctrl-C in the terminal does not reach it:
   the CLI exits and the strategy process ends on its own within its time and CPU limits.
3. **`protocol build` runs the project's own toolchain** (`cargo`, `cc`, `javac`, ...). Build scripts
   and procedural macros run arbitrary code. Only build code you trust.
4. **No real transaction simulation**, no signing, no wallets, no chain access.
5. **No audit.** Treat everything as pre-alpha.

## Verifying strategies (process)

1. Read the whole source and the `strategy.toml` permissions (`protocol verify <dir>` prints both
   the hash and the permissions).
2. Check dependencies, build scripts, network or file access, and anything that spawns processes.
3. Run `protocol test` and `protocol simulate` in both scenarios.
4. `protocol verify <dir> --pin`. This records `name version sha256` in `verified.list`.

"Verified" means **reviewed**. It does not mean safe, bug-free or profitable, and the UI says so.
Verification covers source files only (build output is excluded); rebuild from verified source.
Reviewing your own code is easy to get wrong: prefer a second reviewer.

**Governance note.** If only the founders verify strategies, they are a centralized chokepoint,
which is acceptable for an early project but conflicts with the long-term "no privileged operator"
goal. Move toward multiple independent reviewers and signed, multi-party approvals
(for example two reviewers required) before the marketplace phase. Pins are local to each user;
a shared, signed registry is proposed work.

## Rules for contributors (code this repo)

- Never add key handling to strategy-facing code or to the strategy process environment.
- Never pass secrets in strategy input; never log secrets.
- Do not add a dependency without understanding why (ADR 0003).
- Anything that enables real-asset execution needs its own design document and review first.
- Keep new features simulation-only until the audit and legal gates in the roadmap are met.

## Reporting vulnerabilities

Please report privately to the maintainers before public disclosure. (Contact channel: to be set
when the public repository is created.)

## Live testnet data (read-only)
`protocol aave account` and `protocol watch` read an Aave account from a Sepolia node. They use a
single `eth_call`; there is no signing, no transaction sending and no key anywhere. The RPC URL
comes from `PROTOCOL_RPC_URL` only and is removed from error messages; any chain other than
Sepolia is refused. `protocol aave preflight` additionally asks the node to execute a Pool action
(supply, withdraw, borrow, repay) as if the account had sent it; the node runs it and nothing is
broadcast, so no key and no signature exist. `watch` runs strategies in manual mode, so fund-moving proposals are never
executed. Strategies themselves still have no network access (sandbox).
