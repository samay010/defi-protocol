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
| Process limits | per-tick timeout (killed), 64 KiB output cap, 20 proposals/tick, empty environment |
| Strict parsing | malformed or truncated strategy output fails the tick |
| Fault isolation | errors/panics caught per tick; restart, then mark failed; other instances unaffected |
| Audit log | append-only `audit.log` of every decision with the reason |
| Safe paths | instance and strategy names are restricted (no `/`, `..`); symlinks are not followed when hashing |
| Atomic state writes | temp file + rename |
| Verification pinning | name + version + SHA-256 of the source; any change un-verifies |

## What is NOT implemented (read this)

1. **No OS sandbox.** An external strategy is an ordinary child process of the user. The empty
   environment, timeout and output cap limit *accidents*, not an adversary: a malicious strategy
   could still read your files, open network connections, or fork background processes as your
   user. **Do not run strategies you have not read.** The CLI says so for unverified strategies.
   Planned: WASM/WASI for Rust/C/C++ (no file/network access by default) and container / seccomp /
   namespace isolation for Python and Java.
2. **Memory limits** are not enforced (only time and output size).
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
