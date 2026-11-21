# 0008: Instance lock and process limits (std only)

**Status:** Accepted

**Context.** Review of v0.1 found real problems: (1) two `protocol run` processes could both pass the
"already running" check (a status-file read) and run the same instance, corrupting its state;
(2) after a crash or `kill -9` the status stayed `running` forever; (3) on timeout only the strategy's
direct process was killed, so anything it forked kept running; (4) no CPU or memory limits.

**Decision.**
- `run.lock` per instance, an advisory file lock via `std::fs::File::try_lock` (Rust 1.89+). `run`
  holds it for its lifetime; the OS drops it if the process dies. The lock, not the status file, says
  whether an instance is running. `ps`/`inspect` show a dead run as `stale`. `run` retries the lock
  for about 125 ms so a momentary `ps` probe is not mistaken for a running instance.
- External strategies are started through a small `/bin/sh` wrapper that applies `ulimit -t`
  (always) and `ulimit -v` (only when the manifest sets `max_memory_mb`) and then `exec`s the
  program, in its own process group (`process_group(0)`). On timeout the group is killed with
  `kill -KILL -<pgid>` before the leader is reaped, so the group id cannot have been reused.
- Programs are resolved before spawning: paths must exist, bare names only in the fixed safe PATH.

**Consequences.** + Fixes a real race and the stale-status problem; + forked children die with the
strategy; + no new dependency. - Unix-only (`ulimit`, process groups, `/bin/sh`); on other platforms
the wrapper and group kill are compiled out (that path has not been built or tested) and the lock
is expected to work. - `ulimit -v` limits address
space, not resident memory, and is opt-in because of the JVM. - Not a sandbox: it limits accidents, not
an adversary (see `docs/security.md`). - Ctrl-C no longer reaches the strategy process (own group).
- `kill` is run through `/bin/sh` rather than a syscall because `std` cannot signal a group; a `libc`
  or `nix` call would be cleaner when a dependency is accepted for the sandbox work.
