# 0003: Standard library only in the first release

**Status:** Accepted

**Context.** The brief says not to add dependencies without understanding their purpose. The first
release needs only: argument parsing, a small config format, SHA-256, file persistence, child
processes. It was also built where package downloads are unavailable.

**Decision.** Zero third-party crates. Hand-written: a flag parser (`args.rs`), a strict
`key=value` file format (`kv.rs`), a TOML *subset* for manifests (`manifest.rs`), and SHA-256
(`sha256.rs`, tested against NIST vectors).

**Consequences.** + Nothing to audit upstream, reproducible offline builds. - Hand-rolled parsers
need their own tests and fuzzing; the TOML subset is not full TOML (no arrays/inline tables;
lists are comma-separated strings); a hand-written hash is acceptable for content pinning but
cryptography on real keys must use audited libraries.

**Revisit** when adding networking (HTTP/RPC client), full TOML/JSON, or signing. Each dependency
gets its own ADR entry stating purpose and alternatives.
