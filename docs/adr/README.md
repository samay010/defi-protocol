# Architecture decision records

Short records of decisions that are costly to reverse. New decisions get the next number.
Status values: Accepted, Proposed, Superseded.

| # | Title | Status |
|---|---|---|
| 0001 | Rust core with language-neutral process contract | Accepted |
| 0002 | Off-chain first: no smart contracts, tokens or DAO | Accepted |
| 0003 | Standard library only in the first release | Accepted (partly superseded by 0009) |
| 0004 | Protocol adapter interface | Accepted |
| 0005 | Risk engine as an independent, deny-by-default gate | Accepted |
| 0006 | Simulation-only, testnet-only until audits | Accepted |
| 0007 | Strategy verification by pinned content hash | Accepted |
| 0008 | Instance lock and process limits | Accepted |
| 0009 | `sha2` crate for SHA-256 | Accepted |
| 0010 | `clap` for argument parsing | Accepted |
| 0011 | `serde` + `toml` for strategy manifests | Accepted |
| 0012 | `bubblewrap` OS sandbox for external strategies | Accepted |
| 0013 | Read-only chain access in two layers (alloy transport) | Accepted |
| 0014 | Drop Python and Java strategy support | Accepted |
| 0015 | WebAssembly strategies on the `wasmi` interpreter | Accepted |
