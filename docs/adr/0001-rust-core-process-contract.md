# 0001: Rust core with a language-neutral process contract

**Status:** Accepted

**Context.** The core must be Rust, but strategy authors must not be limited to Rust (Rust, C, C++,
Python, Java at the time; Python and Java were removed later, see ADR 0014). Not every language compiles to WebAssembly.

**Decision.** The engine defines one strategy contract (`strategy.rs`). Official strategies
implement it in-process. All other strategies are processes that speak a small line protocol over
stdin/stdout, one process per tick, with engine-owned state. WASM/WASI is a *later optimization
and isolation mechanism* for the languages that support it, not the contract.

**Consequences.** + Any language with stdio works; + a crash loses no state; + one consistent CLI
experience. - Process start per tick costs latency (fine for monitoring-scale ticks);
- text parsing instead of typed SDK calls (typed SDKs can wrap it later); - isolation is a
separate, still-open piece of work (see `docs/security.md`).

**Alternatives.** gRPC runner per language (heavier, adds dependencies); embedding interpreters
(couples the core to each language, weak isolation); WASM only (excludes Python/Java).
