# 0009: `sha2` crate for SHA-256

**Status:** Accepted. Partially supersedes ADR 0003 (std-only first release).

**Context.** The first release hand-wrote SHA-256 (tested against the NIST vectors) to stay
dependency-free. Hand-written cryptography is a poor thing to ship, even for content pinning, and the
audit plan wants well-reviewed, widely used primitives everywhere a hash or signature matters.

**Decision.** Use the RustCrypto `sha2` crate (resolved to 0.11.1 by `cargo add`; `Cargo.lock` is
committed). The module `core/src/sha256.rs` keeps its previous API (`Sha256`, `sha256_hex`, `hex`),
so `hash.rs` and every existing `verified.list` pin are unchanged. The NIST-vector and
million-`a` tests are kept and pass against the crate, which also shows the output is identical to the
previous implementation.

**Footprint.** `cargo add` locked 10 packages in total: `sha2`, `digest`, `crypto-common`,
`block-buffer`, `hybrid-array`, `typenum`, `const-oid`, `cpufeatures`, `cfg-if`, `libc`.
Licences are expected to be MIT and/or Apache-2.0; confirm with `cargo deny check licenses` and record
the result in `deny.toml` (not yet added).

**Consequences.** + A widely used, community-reviewed implementation with hardware acceleration;
+ less code for our own audit. Note: this is not a claim of an independent audit of `sha2`; check
its current review status before relying on it for anything beyond content pinning. - Ten transitive packages enter the supply chain, so `cargo audit` / `cargo deny`
should run in CI. Other planned swaps (`clap`, `toml`) get their own ADR entries.
